use super::skbuff::netlink_overrun;
use super::{NLmsgFlags, NLmsgType, NLmsghdr, VecExt, MAX_LINKS};
use crate::driver::base::uevent::kobject_uevent::UEVENT_SEQNUM;
use crate::init::initcall::INITCALL_CORE;
use crate::libs::mutex::Mutex;
use crate::libs::rwlock::RwLock;
use crate::libs::rwlock::RwLockWriteGuard;
use crate::libs::spinlock::{SpinLock, SpinLockGuard};
use crate::net::socket::netlink::skbuff::SkBuff;
use crate::net::socket::netlink::NetlinkState;
use crate::net::socket::*;
use crate::net::socket::{AddressFamily, Endpoint, Inode, PMSG, Socket};
use crate::net::syscall::SockAddrNl;
use alloc::sync::Arc;
use alloc::{boxed::Box, vec::Vec};
use core::mem;
use core::ops::Deref;
use core::ptr::copy_nonoverlapping;
use core::{fmt::Debug, hash::Hash};
use hashbrown::{HashMap, HashSet};
use lazy_static::lazy_static;
use netlink::netlink_proto::netlink_protocol::KOBJECT_UEVENT;
use netlink::{NETLINK_ADD_MEMBERSHIP, NETLINK_DROP_MEMBERSHIP, NETLINK_PKTINFO, NLMSG_ALIGNTO};
use num::Zero;
use system_error::SystemError;
use unified_init::macros::unified_init;

bitflags! {
    pub struct NetlinkFlags: u32 {
        const KERNEL_SOCKET = 0x1;
        const RECV_PKTINFO = 0x2;
        const BROADCAST_SEND_ERROR = 0x4;
        const RECV_NO_ENOBUFS = 0x8;
        const LISTEN_ALL_NSID = 0x10;
        const CAP_ACK = 0x20;
        const EXT_ACK = 0x40;
        const STRICT_CHK = 0x80;
        const NETLINK_F_KERNEL_SOCKET = 0x100;
    }
}

type NetlinkSockComparator = Arc<dyn Fn(&NetlinkSock) -> bool + Send + Sync>;
/// 每一个netlink协议族都有一个 NetlinkTable，用于保存该协议族的所有 netlink 套接字
pub struct NetlinkTableItem {
    /// 快速查找和管理套接字
    pub hash: HashMap<u32, Arc<NetlinkSock>>,
    listeners: Option<Listeners>,
    registered: u32,
    flags: u32,
    groups: u32,
    mc_set: HashSet<u32>,
    pub bind: Option<Arc<dyn Fn(i32) -> i32 + Send + Sync>>,
    pub unbind: Option<Arc<dyn Fn(i32) -> i32 + Send + Sync>>,
    pub compare: Option<NetlinkSockComparator>,
}
impl NetlinkTableItem {
    fn new() -> NetlinkTableItem {
        NetlinkTableItem {
            hash: HashMap::new(),
            listeners: Some(Listeners { masks: vec![0; 32] }),
            registered: 0,
            flags: 0,
            groups: 32,
            mc_set: HashSet::new(),
            bind: None,
            unbind: None,
            compare: None,
        }
    }
    pub fn set_registered(&mut self, registered: u32) {
        self.registered = registered;
    }
    pub fn set_flags(&mut self, flags: u32) {
        self.flags = flags;
    }
    pub fn set_groups(&mut self, groups: u32) {
        self.groups = groups;
    }
    pub fn get_registered(&self) -> u32 {
        self.registered
    }
    // todo：异步
    // fn set_callbacks(&mut self, cfg: NetlinkKernelCfg) {
    //     self.bind = cfg.bind;
    //     self.unbind = cfg.unbind;
    //     self.compare = cfg.compare;
    // }
}

// https://code.dragonos.org.cn/xref/linux-6.1.9/net/netlink/af_netlink.c#2916
/// netlink 协议的最大数量
#[unified_init(INITCALL_CORE)]
/// netlink 协议的初始化函数
fn netlink_proto_init() -> Result<(), SystemError> {
    // 创建NetlinkTable,每种netlink协议类型占数组中的一项，后续内核中创建的不同种协议类型的netlink都将保存在这个表中，由该表统一维护
    // 检查NetlinkTable的大小是否符合预期
    let mut nl_table = NL_TABLE.write();
    // let mut nl_table = [0; MAX_LINKS];
    if nl_table.is_empty() {
        panic!("netlink_init: Cannot allocate nl_table");
    }
    // 初始化哈希表
    for i in 0..MAX_LINKS {
        nl_table[i].hash = HashMap::new();
    }
    Ok(())
}

// https://code.dragonos.org.cn/xref/linux-6.1.9/net/netlink/af_netlink.c#572
/// 内核套接字插入 nl_table
pub fn netlink_insert(nlk: Arc<NetlinkSock>, portid: u32) -> Result<(), SystemError> {
    let mut nl_table: RwLockWriteGuard<Vec<NetlinkTableItem>> = NL_TABLE.write();
    let index;
    {
        let mut nlk_guard = nlk.inner.lock();
        index = nlk_guard.protocol;
        // 检查端口是否已经被绑定
        if nlk_guard.bound || nl_table[index].hash.contains_key(&portid) {
            return Err(SystemError::EADDRINUSE);
        }
        // 设置套接字的端口号
        nlk_guard.portid = portid;
        // 设置套接字已绑定
        nlk_guard.bound = portid != 0;
    } // 释放 nlk_guard 锁

    // 将套接字插入哈希表
    nl_table[index].hash.insert(portid, nlk);

    Ok(())
}

/// 自动为netlink套接字选择一个端口号，并在 netlink table 中插入这个端口。如果端口已经被使用，它会尝试使用不同的端口号直到找到一个可用的端口。如果有多个线程同时尝试绑定，则认为是正常情况，并成功返回.
fn netlink_autobind(nlk: Arc<NetlinkSock>, portid: &mut u32) {
    let mut rover: u32 = 0;
    loop {
        let ret = netlink_lookup(nlk.inner.lock().protocol, *portid);

        // 如果查询成功
        if ret.is_some() {
            // 如果 rover 是 0，重置为 1
            if rover == 0 {
                // todo：随机
                rover = 1; // 在 Rust 中不能有 -4096 这样的u32值，因此我们从 1 开始递减
            } else {
                // 否则递减 rover
                rover -= 1;
            }
            *portid = rover;
        } else {
            // 如果查询失败，增加 rover
            rover += 1;
            *portid = rover;
            break;
        }
    }

    netlink_insert(Arc::clone(&nlk), *portid).expect("netlink_insert failed");
}
// TODO: net namespace支持
// https://code.dragonos.org.cn/xref/linux-6.1.9/net/netlink/af_netlink.c#532
/// 在 netlink_table_item 中查找 netlink 套接字
fn netlink_lookup(protocol: usize, portid: u32) -> Option<Arc<NetlinkSock>> {
    // todo: net 支持
    let nl_table = NL_TABLE.read();
    let index = protocol;
    let sk = nl_table[index].hash.get(&portid).unwrap();
    Some(Arc::clone(sk))
}

/* linux：struct sock has to be the first member of netlink_sock */
// linux 6.1.9中的netlink_sock结构体里，sock是一个很大的结构体，这里简化
// 意义是：netlink_sock（NetlinkSock）是一个sock（NetlinkSocket）, 实现了 Netlinksocket trait 和 Sock trait.
#[derive(Debug, Clone)]
pub struct NetlinkSockinner {
    pub portid: u32,
    pub ngroups: u32,
    pub groups: [u32; 32],
    pub bound: bool,
    pub protocol: usize,
    pub subscriptions: u32,
    pub flags: u32,
}
impl NetlinkSockinner {
    fn new(_protocol: usize) -> NetlinkSockinner {
        NetlinkSockinner {
            portid: 0,
            ngroups: 0,
            groups: [0; 32],
            bound: false,
            protocol: _protocol,
            subscriptions: 0,
            flags: 0,
        }
    }
    fn equals(&self, other: SpinLockGuard<NetlinkSockinner>) -> bool {
        self.portid == other.portid
    }
}
#[derive(Debug)]
#[cast_to([sync] Socket)]
pub struct NetlinkSock {
    pub inner: SpinLock<NetlinkSockinner>,
    state: NetlinkState,
    max_recvmsg_len: usize,
    dump_done_errno: i32,
    cb_running: bool,
    queue: Vec<Arc<RwLock<SkBuff>>>,
    data: Arc<Mutex<Vec<Vec<u8>>>>,
    sk_sndtimeo: i64,
    sk_rcvtimeo: i64,
}
impl Clone for NetlinkSock {
    fn clone(&self) -> Self {
        NetlinkSock {
            inner: SpinLock::new((self.inner.lock().deref()).clone()),
            state: self.state,
            max_recvmsg_len: self.max_recvmsg_len,
            dump_done_errno: self.dump_done_errno,
            cb_running: self.cb_running,
            queue: self.queue.clone(),
            data: Arc::clone(&self.data),
            sk_sndtimeo: self.sk_sndtimeo,
            sk_rcvtimeo: self.sk_rcvtimeo,
        }
    }
}
impl Socket for NetlinkSock {
    fn connect(&self, _endpoint: Endpoint) -> Result<(), SystemError> {
        self.netlink_connect(_endpoint)
    }
    fn shutdown(&self, _type: ShutdownTemp) -> Result<(), SystemError> {
        todo!()
    }
    fn bind(&self, _endpoint: Endpoint) -> Result<(), SystemError> {
        log::debug!("NetlinkSock bind to {:?}", _endpoint);
        match _endpoint {
            Endpoint::Netlink(netlinkendpoint) => {
                let addr = netlinkendpoint.addr;
                let sock = Arc::new((*self).clone());
                return sock.netlink_bind(&addr);
            }
            _ => {
                return Err(SystemError::EINVAL);
            }
        }
    }
    fn close(&self) -> Result<(), SystemError> {
        Ok(())
    }
    fn listen(&self, _backlog: usize) -> Result<(), SystemError> {
        todo!()
    }
    fn accept(&self) -> Result<(Arc<Inode>, Endpoint), SystemError> {
        todo!()
    }

    fn wait_queue(&self) -> &WaitQueue {
        todo!()
    }

    fn poll(&self) -> usize {
        todo!()
    }
    fn send_to(
        &self,
        buffer: &[u8],
        _flags: PMSG,
        address: Endpoint,
    ) -> Result<usize, SystemError> {
        log::debug!("NetlinkSock send_to");
        return self.netlink_send(buffer, address);
    }
    fn recv_from(
        &self,
        msg: &mut [u8],
        flags: PMSG,
        _address: Option<Endpoint>,
    ) -> Result<(usize, Endpoint), SystemError> {
        log::debug!("NetlinkSock recv_from，self: {:?}", self);
        return self.netlink_recv(msg, flags);
    }
    fn send_buffer_size(&self) -> usize {
        log::warn!("send_buffer_size is implemented to 0");
        0
    }
    fn recv_buffer_size(&self) -> usize {
        log::warn!("recv_buffer_size is implemented to 0");
        0
    }
    fn set_option(&self, level: PSOL, name: usize, val: &[u8]) -> Result<(), SystemError> {
        return self.netlink_setsockopt(level, name, val);
    }
}

impl NetlinkSock {
    pub fn new(_protocol: Option<usize>) -> NetlinkSock {
        let data: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));
        let sock = NetlinkSock {
            inner: SpinLock::new(NetlinkSockinner::new(_protocol.unwrap_or(0))),
            state: NetlinkState::Unconnected,
            max_recvmsg_len: 0,
            dump_done_errno: 0,
            cb_running: false,
            queue: Vec::new(),
            data,
            sk_sndtimeo: 0,
            sk_rcvtimeo: 0,
        };
        log::debug!("NetlinkSock created: {:?}", sock);
        sock
    }
    // https://code.dragonos.org.cn/xref/linux-6.1.9/net/netlink/af_netlink.c#1078
    fn netlink_connect(&self, _endpoint: Endpoint) -> Result<(), SystemError> {
        Ok(())
    }

    fn netlink_bind(self: Arc<NetlinkSock>, addr: &SockAddrNl) -> Result<(), SystemError> {
        let nladdr = addr;
        let mut nlk_guard = self.inner.lock();
        nlk_guard.portid = addr.nl_pid;
        let mut groups: u32;
        if nladdr.nl_family != AddressFamily::Netlink {
            log::warn!("netlink_bind: nl_family != AF_NETLINK");
            return Err(SystemError::EINVAL);
        }
        groups = nladdr.nl_groups;
        log::info!("netlink_bind: groups: {}", groups);
        if groups != 0 {
            let group_count = addr.nl_groups.count_ones();
            nlk_guard.ngroups = group_count;
            drop(nlk_guard);
            Arc::clone(&self)
                .netlink_realloc_groups()
                .expect("netlink_realloc_groups failed");
            nlk_guard = self.inner.lock();
        }

        if nlk_guard.ngroups < 32 {
            groups &= (1 << nlk_guard.ngroups) - 1;
        }

        let bound = nlk_guard.bound;
        log::info!("netlink_bind: bound: {}", bound);
        if bound {
            if nladdr.nl_pid != nlk_guard.portid {
                return Err(SystemError::EINVAL);
            }
        }

        if !bound {
            drop(nlk_guard);
            if nladdr.nl_pid != 0 {
                netlink_insert(Arc::clone(&self), nladdr.nl_pid)?;
            } else {
                log::info!("netlink_bind: autobind");
                let mut nlk_guard = self.inner.lock();
                netlink_autobind(Arc::clone(&self), &mut nlk_guard.portid);
            }
            nlk_guard = self.inner.lock();
        }

        if nladdr.nl_groups == 0 && (nlk_guard.groups.is_empty() || nlk_guard.groups[0] == 0) {
            log::info!("netlink_bind: no groups");
            return Ok(());
        }

        let new_subscriptions = nlk_guard.subscriptions + nladdr.nl_groups.count_ones()
            - nlk_guard.groups[0].count_ones();
        nlk_guard.groups[0] = groups;
        drop(nlk_guard);
        Arc::clone(&self).netlink_update_subscriptions(new_subscriptions);
        Arc::clone(&self).netlink_update_listeners();
        Ok(())
    }

    // https://code.dragonos.org.cn/xref/linux-6.1.9/net/netlink/af_netlink.c#1849
    /// 用户进程对netlink套接字调用 sendmsg() 系统调用后，内核执行netlink操作的总入口函数
    /// ## 参数
    /// - self    - 指向用户进程的netlink套接字，也就是发送方的
    /// - data     - 承载了发送方传递的netlink消息
    /// - address  - 接收方的地址端点信息
    /// ## 备注
    /// netlink套接字在创建的过程中(具体是在 netlink_create 开头)，已经和 netlink_ops (socket层netlink协议族的通用操作集合)关联,其中注册的 sendmsg 回调就是指向本函数
    fn netlink_send(&self, data: &[u8], _address: Endpoint) -> Result<usize, SystemError> {
        // 一个有效的 Netlink 消息至少应该包含一个消息头
        if data.len() < size_of::<NLmsghdr>() {
            log::warn!("netlink_send: data too short, len: {}", data.len());
            return Err(SystemError::EINVAL);
        }
        #[allow(unsafe_code)]
        let header = unsafe { &*(data.as_ptr() as *const NLmsghdr) };
        if header.nlmsg_len > data.len() {
            log::warn!(
                "netlink_send: data too short, nlmsg_len: {}",
                header.nlmsg_len
            );
            return Err(SystemError::ENAVAIL);
        }
        let message_type = header.nlmsg_type;
        let mut buffer = self.data.lock();
        log::info!("netlink_send: buffer.len: {}", buffer.len());
        // buffer.clear();

        let mut msg = Vec::new();
        let new_header = NLmsghdr {
            nlmsg_len: 0,
            nlmsg_type: message_type,
            nlmsg_flags: NLmsgFlags::NLM_F_MULTI,
            nlmsg_seq: header.nlmsg_seq,
            nlmsg_pid: header.nlmsg_pid,
        };
        // 将新消息头序列化到 msg 中
        msg.push_ext(new_header);
        // 将消息体数据追加到 msg 中
        msg.extend_from_slice(data);
        // 确保 msg 的长度按照 4 字节对齐
        msg.align();
        // msg 的开头设置消息长度。
        msg.set_ext(0, msg.len() as u32);
        // 将序列化后的 msg 添加到发送缓冲区 buffer 中
        buffer.push(msg);
        Ok(data.len())
    }

    // https://code.dragonos.org.cn/xref/linux-6.1.9/net/netlink/af_netlink.c#1938
    /// 用户进程对 netlink 套接字调用 recvmsg() 系统调用后，内核执行 netlink 操作的总入口函数
    /// ## 参数
    /// - sock    - 指向用户进程的netlink套接字，也就是接收方的
    /// - msg     - 用于存放接收到的netlink消息
    /// - len     - 用户空间支持的netlink消息接收长度上限
    /// - flags   - 跟本次接收操作有关的标志位集合(主要来源于用户空间)
    fn netlink_recv(
        &self,
        msg: &mut [u8],
        flags: PMSG,
    ) -> Result<(usize, Endpoint), SystemError> {
        log::info!("netlink_recv on : {:?}", self);
        let nlk = self.inner.lock();
        let mut buffer = self.data.lock();
        log::info!("netlink_recv: buffer.len: {}", buffer.len());
        // 检查 buffer 是否为空
        if buffer.is_empty() {
            return Err(SystemError::ENOBUFS);
        }
        // 从 buffer 中取出第一个消息
        let msg_kernel = buffer.remove(0);
        // 判断是否是带外消息，如果是带外消息，直接返回错误码
        if flags == PMSG::OOB {
            log::warn!("netlink_recv: OOB message is not supported");
            return Err(SystemError::EOPNOTSUPP_OR_ENOTSUP);
        }

        // 计算实际要复制的数据长度，不能超过 msg_from 的长度 或 msg 缓冲区的长度
        let actual_len = msg_kernel.len().min(msg.len());

        let copied: usize = if !msg_kernel.is_empty() {
            msg[..actual_len].copy_from_slice(&msg_kernel[..actual_len]);
            actual_len
        } else {
            // 如果没有数据可复制，返回 0 字节被复制
            0
        };

        let endpoint = Endpoint::Netlink(NetlinkEndpoint {
            addr: SockAddrNl {
                nl_family: AddressFamily::Netlink,
                nl_pad: 0,
                nl_pid: nlk.portid,
                nl_groups: 0,
            },
        });

        // 返回复制的字节数和端点信息
        log::debug!("netlink_recv: copied: {}, endpoint: {:?}", copied, endpoint);
        Ok((copied, endpoint))
    }

    pub fn netlink_has_listeners(&self, group: u32) -> i32 {
        log::info!("netlink_has_listeners");
        let mut res = 0;
        let protocol = self.inner.lock().protocol;

        // 获取读锁
        let nl_table = NL_TABLE.read();

        // 检查 protocol 是否在范围内
        if protocol >= nl_table.len() {
            log::error!(
                "Protocol {} is out of bounds, table's len is {}",
                protocol,
                nl_table.len()
            );
            return res;
        }

        // 获取对应的 NetlinkTableItem
        let netlink_table_item = &nl_table[protocol];

        // 检查 listeners 是否存在
        if let Some(listeners) = &netlink_table_item.listeners {
            // 检查 group 是否在范围内
            if group > 0 && (group as usize - 1) < listeners.masks.len() {
                res = listeners.masks[group as usize - 1] as i32;
            } else {
                log::error!(
                    "Group {} is out of bounds, len is {}",
                    group,
                    listeners.masks.len()
                );
            }
        } else {
            log::error!("Listeners for protocol {} are None", protocol);
        }

        res
    }
    // https://code.dragonos.org.cn/xref/linux-6.1.9/net/netlink/af_netlink.c#549
    /// 更新 netlink 套接字的监听者
    fn netlink_update_listeners(self: Arc<NetlinkSock>) {
        log::info!("netlink_update_listeners");
        let nlk = self.inner.lock();
        let mut nl_table = NL_TABLE.write();
        let netlink_table_item = &mut nl_table[nlk.protocol];
        let listeners = netlink_table_item.listeners.as_mut().unwrap();
        listeners.masks.clear();
        log::info!("nlk.ngroups:{}", nlk.ngroups);
        listeners.masks.resize(nlk.ngroups as usize, 0);
        log::info!("nlk.groups:{:?}", nlk.groups);
        for group in &nlk.groups {
            let mask = 1 << (group % 64);
            let idx = group / 64;
            listeners.masks[idx as usize] |= mask;
        }
    }
    // https://code.dragonos.org.cn/xref/linux-6.1.9/net/netlink/af_netlink.c#571
    /// 重新分配 netlink 套接字的组
    fn netlink_realloc_groups(self: Arc<NetlinkSock>) -> Result<(), SystemError> {
        let mut nlk = self.inner.lock();
        let nl_table = NL_TABLE.write();
        let groups = nl_table[nlk.protocol].groups;
        log::info!("nlk.protocol:{},groups:{:?}", nlk.protocol, groups);
        if nl_table[nlk.protocol].registered == 0 {
            // 没有注册任何组
            log::warn!("netlink_realloc_groups: not registered");
            return Err(SystemError::ENOENT);
        }
        if nlk.ngroups >= groups {
            // 当前已分配的组数量 大于或等于 groups（当前协议的组数量），则没有必要重新分配
            log::info!("netlink_realloc_groups: no need to realloc");
            return Ok(());
        }
        log::info!("nlk.ngroups:{},groups:{}", nlk.ngroups, groups);
        let mut new_groups = [0; 32];
        // 当 nlk.ngroups 大于 0 时复制数据
        if nlk.ngroups > 0 {
            new_groups.copy_from_slice(&nlk.groups);
        }
        nlk.groups = new_groups;
        nlk.ngroups = groups;
        log::info!("nlk.groups:{:?}", nlk.groups);
        Ok(())
    }

    fn netlink_update_subscriptions(self: Arc<NetlinkSock>, subscriptions: u32) {
        log::info!("netlink_update_subscriptions");
        let mut nlk = self.inner.lock();
        let mut nl_table = NL_TABLE.write();
        let netlink_table_item = &mut nl_table[nlk.protocol];

        if nlk.subscriptions != 0 && subscriptions == 0 {
            // 当前有订阅且新的订阅为零，删除绑定节点
            netlink_table_item.mc_set.remove(&nlk.portid);
        } else if nlk.subscriptions == 0 && subscriptions != 0 {
            // 当前没有订阅且新的订阅非零，添加绑定节点
            netlink_table_item.mc_set.insert(nlk.portid);
        }

        // 更新订阅状态
        nlk.subscriptions = subscriptions;
    }
    /// 设置 netlink 套接字的选项
    fn netlink_setsockopt(
        self: &Self,
        level: PSOL,
        optname: usize,
        optval: &[u8],
    ) -> Result<(), SystemError> {
        log::info!("netlink_setsockopt");
        if level != PSOL::NETLINK {
            return Err(SystemError::ENOPROTOOPT);
        }
        let optlen = optval.len();
        let mut val: usize = 0;
        if optlen >= size_of::<usize>() {
            unsafe {
                if optval.len() >= size_of::<usize>() {
                    // 将 optval 中的数据拷贝到 val 中
                    copy_nonoverlapping(
                        optval.as_ptr(),
                        &mut val as *mut usize as *mut u8,
                        size_of::<usize>(),
                    );
                } else {
                    return Err(SystemError::EFAULT);
                }
            }
        } else {
            return Err(SystemError::EINVAL);
        }
        match optname {
            // add 和 drop 对应同一段代码
            NETLINK_ADD_MEMBERSHIP | NETLINK_DROP_MEMBERSHIP => {
                let group = val as u64;
                let mut nl_table = NL_TABLE.write();
                let netlink_table_item = &mut nl_table[self.inner.lock().protocol];
                let listeners = netlink_table_item.listeners.as_mut().unwrap();
                let group = group - 1;
                let mask = 1 << (group % 64);
                let idx = group / 64;
                if optname == NETLINK_ADD_MEMBERSHIP {
                    listeners.masks[idx as usize] |= mask;
                } else {
                    listeners.masks[idx as usize] &= !mask;
                }
            }
            NETLINK_PKTINFO => {
                todo!();
            }
            _ => {
                return Err(SystemError::ENOPROTOOPT);
            }
        }
        Ok(())
    }
    // 接收缓冲区的已分配内存
    fn sk_rmem_alloc(&self) -> usize {
        0
    }
    // 接收缓冲区的大小
    fn sk_rcvbuf(&self) -> usize {
        self.max_recvmsg_len
    }
    /// 尝试向指定用户进程 netlink 套接字发送组播消息
    /// ## 参数：
    /// - sk: 指向一个 sock 结构，对应一个用户进程 netlink 套接字
    /// - info: 指向一个 netlink 组播消息的管理块
    /// ## 备注：
    /// 传入的 netlink 套接字跟组播消息属于同一种 netlink 协议类型，并且这个套接字开启了组播阅订，除了这些，其他信息(比如阅订了具体哪些组播)都是不确定的
    /// TODO: net namespace
    fn do_one_broadcast(
        self: Arc<Self>,
        info: &mut Box<NetlinkBroadcastData>,
    ) -> Result<(), SystemError> {
        // 如果源 sock 和目的 sock 是同一个则直接返回
        if info.exclude_sk.inner.lock().equals(self.inner.lock()) {
            log::info!("do_one_broadcast: exclude_sk equals sk");
            return Err(SystemError::EINVAL);
        }

        let nlk_guard = self.inner.lock();
        log::info!("do_one_broadcast: nlk.portid: {}", nlk_guard.portid);
        log::info!(
            "do_one_broadcast: info.group: {}, nlk_guard.ngroups: {}, nlk_guard.groups: {:?}",
            info.group,
            nlk_guard.ngroups,
            nlk_guard.groups
        );
        // 如果目的单播地址就是该 netlink 套接字
        // 或者目的组播地址超出了该 netlink 套接字的上限
        // 或者该 netlink 套接字没有阅订这条组播消息，都直接返回
        if nlk_guard.portid == info.portid
            || info.group > nlk_guard.ngroups
            || !nlk_guard.groups.contains(&(info.group - 1))
        {
            log::warn!("do_one_broadcast: portid or group error");
            return Err(SystemError::EINVAL);
        }
        // 如果 netlink 组播消息的管理块携带了 failure 标志, 则对该 netlink 套接字设置缓冲区溢出状态
        if info.failure != 0 {
            log::warn!("do_one_broadcast: failure");
            netlink_overrun(&self);
            return Err(SystemError::EINVAL);
        }
        // 设置 skb2，其内容来自 skb
        if info.skb_2.inner.lock().is_empty() {
            if info.skb.skb_shared() {
                info.copy_skb_to_skb_2();
            } else {
                info.skb_2 = info.skb.clone();
                info.skb_2.skb_orphan();
            }
        }
        // 到这里如果 skb2 还是 NULL，意味着上一步中 clone 失败
        if info.skb_2.inner.lock().is_empty() {
            log::warn!("do_one_broadcast: skb_2 is empty");
            netlink_overrun(&self);
            info.failure = 1;
            if (nlk_guard.flags != 0) & (!NetlinkFlags::BROADCAST_SEND_ERROR.bits().is_zero()) {
                info.delivery_failure = 1;
            }
            return Err(SystemError::EINVAL);
        }

        if info.skb_2.sk_filter(&self) {
            return Err(SystemError::EINVAL);
        }
        log::info!("do_one_broadcast: sk_filter success");
        drop(nlk_guard);
        let ret = Arc::clone(&self).netlink_broadcast_deliver(&mut info.skb_2);
        let nlk_guard = self.inner.lock();
        // 如果将承载了组播消息的 skb 发送到该用户进程 netlink 套接字失败
        if ret.is_err() {
            netlink_overrun(&self);
            if nlk_guard.flags != 0 && !NetlinkFlags::BROADCAST_SEND_ERROR.bits().is_zero() {
                info.delivery_failure = 1;
            }
        } else {
            // info.congested |= -1;
            info.delivered = 1;
            info.skb_2 = info.skb.clone();
        }

        drop(nlk_guard);
        log::info!("do_one_broadcast success");
        Ok(())
    }
    // https://code.dragonos.org.cn/xref/linux-6.1.9/net/netlink/af_netlink.c#1499
    /// 发送 netlink 组播消息
    /// ## 参数
    /// - ssk: 源 sock
    /// - skb: 属于发送方的承载了netlink消息的skb
    /// - portid: 目的单播地址
    /// - group: 目的组播地址
    ///
    /// ## 备注: 以下2种情况都会调用到本函数：
    ///  [1]. 用户进程   --组播--> 用户进程
    ///  [2]. kernel     --组播--> 用户进程
    ///
    pub fn netlink_broadcast(
        self: &Arc<Self>,
        skb: SkBuff,
        portid: u32,
        group: u32,
    ) -> Result<(), SystemError> {
        log::info!("netlink_broadcast");

        let mut info = Box::new(NetlinkBroadcastData {
            exclude_sk: self,
            portid,
            group,
            failure: 0,
            delivery_failure: 0,
            congested: 0,
            delivered: 0,
            skb,
            skb_2: SkBuff::new(None),
        });
        let protocol = self.inner.lock().protocol;
        // While we sleep in clone, do not allow to change socket list
        let nl_table = NL_TABLE.read();
        let mc_set = nl_table[protocol].mc_set.clone();
        drop(nl_table);

        // 遍历 netlink_table_item 中的所有 netlink 套接字，尝试向每一个套接字发送组播消息
        for portid in &mc_set {
            match netlink_lookup(protocol, *portid) {
                Some(usk) => {
                    log::info!("netlink_lookup: usk.portid: {}", usk.inner.lock().portid);
                    if let Err(e) = Arc::clone(&usk).do_one_broadcast(&mut info) {
                        log::error!("Failed to broadcast to portid {}: {:?}", portid, e);
                    }
                }
                None => {
                    log::warn!("No NetlinkSock found for portid: {}", portid);
                }
            }
        }
        drop(info.skb);

        if info.delivery_failure != 0 {
            return Err(SystemError::ENOBUFS);
        }
        drop(info.skb_2);

        log::info!(
            "info.delivered: {}, info.congested: {}",
            info.delivered,
            info.congested
        );
        // todo：实现非阻塞
        return Err(SystemError::ESRCH);
    }

    // https://code.dragonos.org.cn/xref/linux-6.1.9/net/netlink/af_netlink.c?fi=netlink_has_listeners#1400
    /// 处理Netlink套接字的广播消息传递
    /// - 将携带了 netlink 组播消息的 skb 发送到指定目的用户进程 netlink 套接字
    ///
    /// ## 参数
    /// - sk: 指向一个 sock 结构，对应一个用户进程 netlink 套接字
    /// - skb: 指向一个网络缓冲区 skb，携带了 netlink 组播消息
    ///
    /// ## 返回值      
    ///  - -1: 套接字接收条件不满足
    ///  - 0: netlink组播消息发送成功，套接字已经接收，尚未处理数据长度小于等于其接收缓冲的1/2
    ///  - 1: netlink组播消息发送成功，套接字已经接收但尚未处理数据长度大于其接收缓冲的1/2(这种情况似乎意味着套接字处于拥挤状态)
    ///
    /// ## 备注：
    /// - 到这里，已经确定了传入的 netlink 套接字跟组播消息匹配正确；
    fn netlink_broadcast_deliver(self: Arc<Self>, skb: &mut SkBuff) -> Result<i32, SystemError> {
        log::info!("netlink_broadcast_deliver");

        {
            // 如果接收缓冲区的已分配内存小于或等于其总大小，并且套接字没有被标记为拥塞，则继续执行内部的代码块。
            if (self.sk_rmem_alloc() <= self.sk_rcvbuf())
                && !(self.state == NetlinkState::SCongested)
            {
                // 如果满足接收条件，则设置skb的所有者是该netlink套接字
                skb.netlink_skb_set_owner_r(Arc::clone(&self));
            } else {
                return Err(SystemError::EINVAL);
            }
        }

        // 将 skb 发送到该 netlink 套接字
        let _ = self.clone().netlink_sendskb(skb);

        {
            // 如果套接字的接收缓冲区已经接收但尚未处理数据长度大于其接收缓冲的1/2，则返回1
            if self.sk_rmem_alloc() > (self.sk_rcvbuf() >> 1) {
                log::warn!("Socket buffer is more than half full, returning ENOBUFS");
                return Err(SystemError::ENOBUFS);
            } else {
                log::info!("Uevent sent successfully, returning 0");
                return Ok(0);
            }
        }
    }

    // https://code.dragonos.org.cn/xref/linux-6.1.9/net/netlink/af_netlink.c?fi=netlink_has_listeners#1268
    /// 将一个网络缓冲区 skb 中的数据发送到指定的目标进程套接字
    fn netlink_sendskb(self: Arc<Self>, skb: &SkBuff) -> u32 {
        let mut combined_message = Vec::new();
        {
            let inner_data = skb.inner.lock();
            for segment in inner_data.iter() {
                combined_message.extend_from_slice(segment);
                log::info!("netlink_sendskb: combined_message: {:?}", combined_message);
            }
        }

        // align the combined message to 4 bytes required by Netlink protocol
        while combined_message.len() % NLMSG_ALIGNTO != 0 {
            combined_message.push(0);
        }
        // Create a new Netlink header
        let new_header = NLmsghdr {
            nlmsg_len: (combined_message.len() + mem::size_of::<NLmsghdr>()),
            nlmsg_type: NLmsgType::NLMSG_DONE,
            nlmsg_flags: NLmsgFlags::NLM_F_MULTI,
            nlmsg_seq: UEVENT_SEQNUM,
            nlmsg_pid: 0, // 来自内核
        };

        // Serialize header and message into the final buffer
        let mut final_msg = Vec::new();
        final_msg.push_ext(new_header); // Add header
        final_msg.align(); // Align the message to 4 bytes
        final_msg.extend(combined_message); // Add the flattened message content
        final_msg.set_ext(0, final_msg.len() as u32); // Set the length of the message
                                                      // Update sk_guard.data to contain the final message
        let mut buffer = self.data.lock();
        log::info!("netlink_sendskb: buffer.len: {}", buffer.len());
        // buffer.clear();
        buffer.push(final_msg.clone());
        log::info!("netlink_sendskb: buffer: {:?}", buffer.deref());

        final_msg.len() as u32 // Return the length of the final message
    }
}

#[derive(Clone)]
pub struct Listeners {
    // 组播组掩码，每一位代表一个组播组，如果对应位为 1，表示有监听
    masks: Vec<u64>,
}
impl Listeners {
    // 创建一个新的 `Listeners` 实例，并将 `masks` 的所有位初始化为 0
    pub fn new() -> Listeners {
        let masks = vec![0u64; 32];
        Listeners { masks }
    }
}

fn initialize_netlink_table() -> RwLock<Vec<NetlinkTableItem>> {
    let mut tables = Vec::with_capacity(MAX_LINKS);
    for _ in 0..MAX_LINKS {
        tables.push(NetlinkTableItem::new());
    }
    // 以下进行 Netlink 协议注册
    tables[KOBJECT_UEVENT].set_registered(1);
    RwLock::new(tables)
}

lazy_static! {
    /// 一个维护全局的 NL_TABLE 向量，每一个元素代表一个 netlink 协议类型，最大数量为 MAX_LINKS
    pub static ref NL_TABLE: RwLock<Vec<NetlinkTableItem>> = initialize_netlink_table();
}

struct NetlinkBroadcastData<'a> {
    exclude_sk: &'a Arc<NetlinkSock>,
    portid: u32,
    group: u32,
    failure: i32,
    delivery_failure: i32,
    congested: i32,
    delivered: i32,
    skb: SkBuff,
    skb_2: SkBuff,
}
impl<'a> NetlinkBroadcastData<'a> {
    pub fn copy_skb_to_skb_2(&mut self) {
        let skb = self.skb.clone();
        self.skb_2 = skb;
    }
}
