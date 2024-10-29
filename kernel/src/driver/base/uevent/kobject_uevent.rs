use core::error;
use core::ops::Deref;

// https://code.dragonos.org.cn/xref/linux-6.1.9/lib/kobject_uevent.c
use super::KObject;
use super::KobjUeventEnv;
use super::KobjectAction;
use super::{UEVENT_BUFFER_SIZE, UEVENT_NUM_ENVP};
use crate::driver::base::kobject::{KObjectManager, KObjectState};
use crate::init::initcall::INITCALL_POSTCORE;
use crate::libs::mutex::Mutex;
use crate::net::socket::netlink::af_netlink::NetlinkSock;
use crate::net::socket::netlink::netlink_proto::netlink_protocol::KOBJECT_UEVENT;
use crate::net::socket::netlink::skbuff::SkBuff;
use crate::net::socket::netlink::{netlink_kernel_create, NetlinkKernelCfg, NL_CFG_F_NONROOT_RECV};
use alloc::boxed::Box;
use alloc::collections::LinkedList;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use num::Zero;
use system_error::SystemError;
use unified_init::macros::unified_init;
// 全局变量，uevent 消息的序列号
pub static UEVENT_SEQNUM: u64 = 0;

struct UeventSock {
    inner: Arc<NetlinkSock>,
}
impl UeventSock {
    pub fn new(inner: Arc<NetlinkSock>) -> Self {
        UeventSock { inner }
    }
}

// 用于存储所有用于发送 uevent 消息的 netlink sockets。这些 sockets 用于在内核和用户空间之间传递设备事件通知。
// 每当需要发送 uevent 消息时，内核会遍历这个链表，并通过其中的每一个 socket 发送消息。
// 使用 Mutex 保护全局链表
lazy_static::lazy_static! {
    static ref UEVENT_SOCK_LIST: Mutex<LinkedList<UeventSock>> = Mutex::new(LinkedList::new());
}

/// 内核初始化的时候，在设备初始化之前执行
#[unified_init(INITCALL_POSTCORE)]
fn kobejct_uevent_init() -> Result<(), SystemError> {
    // todo: net namespace
    return uevent_net_init();
}
// TODO：等 net namespace 实现后添加 net 参数和相关操作
// 内核启动的时候，即使没有进行网络命名空间的隔离也需要调用这个函数
// 支持 net namespace 之后需要在每个 net namespace 初始化的时候调用这个函数
/// 为每一个 net namespace 初始化 uevent
fn uevent_net_init() -> Result<(), SystemError> {
    let cfg = NetlinkKernelCfg {
        groups: 1,
        flags: NL_CFG_F_NONROOT_RECV,
        ..Default::default()
    };
    // 为 NETLINK_KOBJECT_UEVENT 协议创建一个内核 netlink socket
    let ue_sk = UeventSock::new(netlink_kernel_create(KOBJECT_UEVENT, Some(cfg)).unwrap());

    // 每个 net namespace 向链表中添加一个新的 uevent socket
    UEVENT_SOCK_LIST.lock().push_back(ue_sk);
    log::info!("uevent_net_init finish");
    return Ok(());
}

/// kobject_uevent，和kobject_uevent_env功能一样，只是没有指定任何的环境变量
pub fn kobject_uevent(kobj: Arc<dyn KObject>, action: KobjectAction) -> Result<(), SystemError> {
    match kobject_uevent_env(kobj, action, Vec::new()) {
        Ok(_) => Ok(()),
        Err(e) => Err(e),
    }
}
// https://code.dragonos.org.cn/xref/linux-6.1.9/lib/kobject_uevent.c#309
///  kobject_uevent_env，以 envp 为环境变量，上报一个指定 action 的 uevent。环境变量的作用是为执行用户空间程序指定运行环境。
pub fn kobject_uevent_env(
    kobj: Arc<dyn KObject>,
    action: KobjectAction,
    envp_ext: Vec<String>,
) -> Result<i32, SystemError> {
    log::info!("kobject_uevent_env: kobj: {:?}, action: {:?}", kobj, action);
    let mut state = KObjectState::empty();
    let mut top_kobj = kobj.parent().unwrap().upgrade().unwrap();
    let mut retval: i32;
    let action_string = match action {
        KobjectAction::KOBJADD => "add".to_string(),
        KobjectAction::KOBJREMOVE => "remove".to_string(),
        KobjectAction::KOBJCHANGE => "change".to_string(),
        KobjectAction::KOBJMOVE => "move".to_string(),
        KobjectAction::KOBJONLINE => "online".to_string(),
        KobjectAction::KOBJOFFLINE => "offline".to_string(),
        KobjectAction::KOBJBIND => "bind".to_string(),
        KobjectAction::KOBJUNBIND => "unbind".to_string(),
    };
    /*
     * Mark "remove" event done regardless of result, for some subsystems
     * do not want to re-trigger "remove" event via automatic cleanup.
     */
    if let KobjectAction::KOBJREMOVE = action {
        log::info!("kobject_uevent_env: action: remove");
        state.insert(KObjectState::REMOVE_UEVENT_SENT);
    }

    // 不断向上查找，直到找到最顶层的kobject
    while let Some(weak_parent) = top_kobj.parent() {
        log::info!("kobject_uevent_env: top_kobj: {:?}", top_kobj);
        top_kobj = weak_parent.upgrade().unwrap();
    }
    /* 查找当前kobject或其parent是否从属于某个kset;如果都不从属于某个kset，则返回错误。(说明一个kobject若没有加入kset，是不会上报uevent的) */
    if kobj.kset().is_none() && top_kobj.kset().is_none() {
        log::info!("attempted to send uevent without kset!\n");
        return Err(SystemError::EINVAL);
    }

    let kset = top_kobj.kset();
    // 判断该 kobject 的状态是否设置了uevent_suppress，如果设置了，则忽略所有的uevent上报并返回
    if kobj.kobj_state().contains(KObjectState::UEVENT_SUPPRESS) {
        log::info!("uevent_suppress caused the event to drop!");
        return Ok(0);
    }

    // 如果所属的kset的kset->filter返回的是0，过滤此次上报
    if let Some(kset_ref) = kset.as_ref() {
        if let Some(uevent_ops) = &kset_ref.uevent_ops {
            if uevent_ops.filter() == Some(0) {
                log::info!("filter caused the event to drop!");
                return Ok(0);
            }
        }
    }

    // 判断所属的kset是否有合法的名称（称作subsystem，和前期的内核版本有区别），否则不允许上报uevent
    // originating subsystem
    let subsystem: String = if let Some(kset_ref) = kset.as_ref() {
        if let Some(uevent_ops) = &kset_ref.uevent_ops {
            uevent_ops.uevent_name()
        } else {
            log::info!("kset name: {}", kset_ref.name());
            kset_ref.name()
        }
    } else {
        log::error!("kobject_uevent_env: kset is None");
        kobj.name()
    };
    if subsystem.is_empty() {
        log::info!("unset subsystem caused the event to drop!");
    }
    log::info!("kobject_uevent_env: subsystem: {}", subsystem);

    // 创建一个用于环境变量的缓冲区
    let mut env = Box::new(KobjUeventEnv {
        argv: Vec::with_capacity(UEVENT_NUM_ENVP),
        envp: Vec::with_capacity(UEVENT_NUM_ENVP),
        envp_idx: 0,
        buf: vec![0u8; UEVENT_BUFFER_SIZE],
        buflen: 0,
    });
    // 需要手动填充缓冲区，不然会有非预期的字节！
    let _ = &env.buf.fill(0);
    log::info!("init: buf: {:?}", &env.buf);
    log::info!("init: buf.to_string: {:?}", String::from_utf8_lossy(&env.buf));
    if env.buf.is_empty() {
        log::error!("kobject_uevent_env: failed to allocate buffer");
        return Err(SystemError::ENOMEM);
    }

    // 获取设备的完整对象路径
    let devpath: String = KObjectManager::kobject_get_path(&kobj);
    log::info!("kobject_uevent_env: devpath: {}", devpath);
    if devpath.is_empty() {
        retval = SystemError::ENOENT.to_posix_errno();
        drop(devpath);
        drop(env);
        log::warn!("kobject_uevent_env: devpath is empty");
        return Ok(retval);
    }
    retval = env.add_uevent_var("ACTION=", &action_string).unwrap();
    log::info!("kobject_uevent_env: retval: {}", retval);
    if !retval.is_zero() {
        drop(devpath);
        drop(env);
        log::info!("add_uevent_var failed ACTION");
        return Ok(retval);
    };
    retval = env.add_uevent_var("DEVPATH=", &devpath).unwrap();
    if !retval.is_zero() {
        drop(devpath);
        drop(env);
        log::info!("add_uevent_var failed DEVPATH");
        return Ok(retval);
    };
    retval = env.add_uevent_var("SUBSYSTEM=", &subsystem).unwrap();
    if !retval.is_zero() {
        drop(devpath);
        drop(env);
        log::info!("add_uevent_var failed SUBSYSTEM");
        return Ok(retval);
    };

    /* keys passed in from the caller */

    for var in envp_ext {
        let retval = env.add_uevent_var("", &var).unwrap();
        if !retval.is_zero() {
            drop(devpath);
            drop(env);
            log::info!("add_uevent_var failed");
            return Ok(retval);
        }
    }
    if let Some(kset_ref) = kset.as_ref() {
        if let Some(uevent_ops) = kset_ref.uevent_ops.as_ref() {
            if uevent_ops.uevent(&env) != 0 {
                retval = uevent_ops.uevent(&env);
                if retval.is_zero() {
                    log::info!("kset uevent caused the event to drop!");
                    drop(devpath);
                    drop(env);
                    return Ok(retval);
                }
            }
        }
    }
    match action {
        KobjectAction::KOBJADD => {
            state.insert(KObjectState::ADD_UEVENT_SENT);
        }
        KobjectAction::KOBJUNBIND => {
            KobjUeventEnv::zap_modalias_env(&mut env);
        }
        _ => {}
    }

    /* we will send an event, so request a new sequence number */
    retval =
        KobjUeventEnv::add_uevent_var(&mut env, "SEQNUM=", &(UEVENT_SEQNUM + 1).to_string())
            .unwrap();
    if !retval.is_zero() {
        drop(devpath);
        drop(env);
        log::info!("add_uevent_var failed");
        return Ok(retval);
    }
    retval = kobject_uevent_net_broadcast(kobj, &env, &action_string, &devpath);
    // todo: 设置了 UEVENT_HELP 编译条件之后，使用 handle_uevent_helper() 对指定的 uevent 进行处理，通常是热插拔程序 mdev、udev 等
    drop(devpath);
    drop(env);
    log::info!("kobject_uevent_env: retval: {}", retval);
    return Ok(retval);
}

// 用于处理网络相关的uevent（通用事件）广播
// https://code.dragonos.org.cn/xref/linux-6.1.9/lib/kobject_uevent.c#381
pub fn kobject_uevent_net_broadcast(
    kobj: Arc<dyn KObject>,
    env: &KobjUeventEnv,
    action_string: &str,
    devpath: &str,
) -> i32 {
    // TODO: net namespace
    // 如果有网络命名空间，则广播标记的uevent；如果没有，则广播未标记的uevent
    let ret = uevent_net_broadcast_untagged(env, action_string, devpath);
    log::info!("kobject_uevent_net_broadcast finish. ret: {}", ret);
    ret
}

/// 分配一个用于 uevent 消息的 skb（socket buffer）。此 skb 将包含指定的 action_string 和 devpath。
pub fn alloc_uevent_skb<'a>(
    env: &'a KobjUeventEnv,
    action_string: &'a str,
    devpath: &'a str,
) -> SkBuff {
    let len = action_string.len() + devpath.len() + 2;
    let total_len = len + env.buflen;
    log::info!("alloc_uevent_skb: total_len: {}, len: {}", total_len, len);
    // 分配一个新的 skb
    let skb = SkBuff {
        sk: Arc::new(NetlinkSock::new(None)),
        inner: Arc::new(Mutex::new(Vec::with_capacity(total_len))),
    };
    log::info!("alloc_uevent_skb: skb: {:?}", skb);
    {
        let mut inner = skb.inner.lock();
        // 以下语句推入内容形如：add@/platform/rtc_cmos/rtc0
        // inner.push(format!("{}@{}", action_string, devpath).into_bytes());
        // 以下语句推入内容形如：ACTION=add DEVPATH=/platform/rtc_cmos/rtc0 SUBSYSTEM=rtc SEQNUM=1
        inner.push(env.buf.clone());
    }
    let binding = skb.clone();
    let binding = binding.inner.lock();
    let debug_inner = binding.deref();
    log::info!("alloc_uevent_skb: inner: {:?}", debug_inner);
    skb
}
// https://code.dragonos.org.cn/xref/linux-6.1.9/lib/kobject_uevent.c#309
///  广播一个未标记的 uevent 消息
pub fn uevent_net_broadcast_untagged(
    env: &KobjUeventEnv,
    action_string: &str,
    devpath: &str,
) -> i32 {
    log::info!(
        "uevent_net_broadcast_untagged: action_string: {}, devpath: {}",
        action_string,
        devpath
    );
    let mut retval = 0;

    // 锁定 UEVENT_SOCK_LIST 并遍历
    let ue_sk_list = UEVENT_SOCK_LIST.lock();
    for ue_sk in ue_sk_list.iter() {
        // 如果没有监听者，则跳过
        if ue_sk.inner.netlink_has_listeners(1) == 0 {
            log::info!("uevent_net_broadcast_untagged: no listeners");
            continue;
        }
        // 分配一个新的 skb
        let skb = alloc_uevent_skb(env, action_string, devpath);
        log::info!("next is netlink_broadcast");
        let netlink_socket = Arc::clone(&ue_sk.inner);
        // portid = 0: 表示消息发送给内核或所有监听的进程，而不是特定的用户空间进程。
        // group = 1: 表示消息发送给 netlink 多播组 ID 为 1 的组。在 netlink 广播中，组 ID 1 通常用于 uevent 消息。
        retval = match netlink_socket.netlink_broadcast( skb, 0, 1) {
            Ok(_) => 0,
            Err(err) => err.to_posix_errno(),
        };
        log::info!("finished netlink_broadcast");
        // ENOBUFS should be handled in userspace
        if retval == SystemError::ENOBUFS.to_posix_errno()
            || retval == SystemError::ESRCH.to_posix_errno()
        {
            retval = 0;
        }
    }
    retval
}
