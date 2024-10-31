pub mod af_netlink;
pub mod endpoint;
pub mod netlink_proto;
pub mod skbuff;
use super::{family, Inode, Socket, Type};
use af_netlink::{netlink_insert, Listeners, NetlinkFlags, NetlinkSock, NL_TABLE};
use alloc::sync::Arc;
use alloc::{slice, vec::Vec};
use netlink_proto::netlink_protocol::KOBJECT_UEVENT;
use system_error::SystemError;
// 监听事件类型
pub const NETLINK_ADD_MEMBERSHIP: usize = 1;
pub const NETLINK_DROP_MEMBERSHIP: usize = 2;
pub const NETLINK_PKTINFO: usize = 3; // 接收包信息。如果设置了这个选项，套接字将接收包含发送者信息（如发送者的端口号和地址）的消息
pub const MAX_LINKS: usize = 32; // 内核netlink套接字的最大数量
                                 // 允许非特权用户接收/发送消息
pub const NL_CFG_F_NONROOT_RECV: u32 = 1 << 0;

bitflags! {
/// 四种通用的消息类型 nlmsg_type
pub struct NLmsgType: u8 {
    /* Nothing.     */
    const NLMSG_NOOP = 0x1;
    /* Error       */
    const NLMSG_ERROR = 0x2;
    /* End of a dump    */
    const NLMSG_DONE = 0x3;
    /* Data lost     */
    const NLMSG_OVERRUN = 0x4;
}

//消息标记
pub struct NLmsgFlags: u16 {
    /* Flags values */
    const NLM_F_REQUEST = 0x01;
    const NLM_F_MULTI = 0x02;
    const NLM_F_ACK = 0x04;
    const NLM_F_ECHO = 0x08;
    const NLM_F_DUMP_INTR = 0x10;
    const NLM_F_DUMP_FILTERED = 0x20;

    /* Modifiers to GET request */
    const NLM_F_ROOT = 0x100; /* specify tree root    */
    const NLM_F_MATCH = 0x200; /* return all matching    */
    const NLM_F_ATOMIC = 0x400; /* atomic GET        */
    //const NLM_F_DUMP = NLM_F_ROOT | NLM_F_MATCH;
    const NLM_F_DUMP = 0x100 | 0x200;

    /* Modifiers to NEW request */
    const NLM_F_REPLACE = 0x100; /* Override existing        */
    const NLM_F_EXCL = 0x200; /* Do not touch, if it exists    */
    const NLM_F_CREATE = 0x400; /* Create, if it does not exist    */
    const NLM_F_APPEND = 0x800; /* Add to end of list        */

    /* Modifiers to DELETE request */
    const NLM_F_NONREC = 0x100;	/* Do not delete recursively	*/

     /* Flags for ACK message */
    const NLM_F_CAPPED = 0x100;	/* request was capped */
    const NLM_F_ACK_TLVS = 0x200;	/* extended ACK TVLs were included */
}
}

// 参考 https://code.dragonos.org.cn/xref/linux-6.1.9/include/linux/netlink.h
#[allow(dead_code)]
pub struct NLmsghdr {
    pub nlmsg_len: usize,
    pub nlmsg_type: NLmsgType,
    pub nlmsg_flags: NLmsgFlags,
    pub nlmsg_seq: u32,
    pub nlmsg_pid: u32,
}

const NLMSG_ALIGNTO: usize = 4;
#[derive(Debug, PartialEq, Copy, Clone)]
#[allow(dead_code)]
pub enum NetlinkState {
    Unconnected = 0,
    Connected = 1,
    SCongested = 2,
}

// 定义类型别名来简化闭包类型的定义
type InputCallback = Arc<dyn FnMut() + Send + Sync>;
type BindCallback = Arc<dyn Fn(i32) -> i32 + Send + Sync>;
type UnbindCallback = Arc<dyn Fn(i32) -> i32 + Send + Sync>;
type CompareCallback = Arc<dyn Fn(&NetlinkSock) -> bool + Send + Sync>;
/// 该结构包含了内核 netlink 的可选参数:
#[derive(Default)]
pub struct NetlinkKernelCfg {
    pub groups: u32,
    pub flags: u32,
    pub input: Option<InputCallback>,
    pub bind: Option<BindCallback>,
    pub unbind: Option<UnbindCallback>,
    pub compare: Option<CompareCallback>,
}

impl NetlinkKernelCfg {
    pub fn new() -> Self {
        NetlinkKernelCfg {
            groups: 32,
            flags: 0,
            input: None,
            bind: None,
            unbind: None,
            compare: None,
        }
    }

    pub fn set_input<F>(&mut self, callback: F)
    where
        F: FnMut() + Send + Sync + 'static,
    {
        self.input = Some(Arc::new(callback));
    }

    pub fn set_bind<F>(&mut self, callback: F)
    where
        F: Fn(i32) -> i32 + Send + Sync + 'static,
    {
        self.bind = Some(Arc::new(callback));
    }

    pub fn set_unbind<F>(&mut self, callback: F)
    where
        F: Fn(i32) -> i32 + Send + Sync + 'static,
    {
        self.unbind = Some(Arc::new(callback));
    }

    pub fn set_compare<F>(&mut self, callback: F)
    where
        F: Fn(&NetlinkSock) -> bool + Send + Sync + 'static,
    {
        self.compare = Some(Arc::new(callback));
    }
}

pub trait VecExt {
    fn align(&mut self);
    fn push_ext<T: Sized>(&mut self, data: T);
    fn set_ext<T: Sized>(&mut self, offset: usize, data: T);
}

impl VecExt for Vec<u8> {
    fn align(&mut self) {
        let len = (self.len() + NLMSG_ALIGNTO - 1) & !(NLMSG_ALIGNTO - 1);
        if len > self.len() {
            self.resize(len, 0);
        }
    }

    fn push_ext<T: Sized>(&mut self, data: T) {
        #[allow(unsafe_code)]
        let bytes =
            unsafe { slice::from_raw_parts(&data as *const T as *const u8, size_of::<T>()) };
        for byte in bytes {
            self.push(*byte);
        }
    }

    fn set_ext<T: Sized>(&mut self, offset: usize, data: T) {
        if self.len() < offset + size_of::<T>() {
            self.resize(offset + size_of::<T>(), 0);
        }
        #[allow(unsafe_code)]
        let bytes =
            unsafe { slice::from_raw_parts(&data as *const T as *const u8, size_of::<T>()) };
        self[offset..(bytes.len() + offset)].copy_from_slice(bytes);
    }
}

/// 创建一个新的内核netlink套接字
pub fn netlink_kernel_create(
    unit: usize,
    cfg: Option<NetlinkKernelCfg>,
) -> Result<Arc<NetlinkSock>, SystemError> {
    let nlk: Arc<NetlinkSock> = Arc::new(NetlinkSock::new(Some(unit)));
    let groups: u32;
    if unit >= MAX_LINKS {
        return Err(SystemError::EINVAL);
    }
    __netlink_create(Arc::clone(&nlk), unit, 1).expect("__netlink_create failed");

    if let Some(cfg) = cfg.as_ref() {
        if cfg.groups < 32 {
            groups = 32;
        } else {
            groups = cfg.groups;
        }
    } else {
        groups = 32;
    }
    let listeners = Listeners::new();
    // todo：设计和实现回调函数
    // sk.sk_data_read = netlink_data_ready;
    // if cfg.is_some() && cfg.unwrap().input.is_some(){
    //     nlk.netlink_rcv = cfg.unwrap().input;
    // }

    // 插入内核套接字
    netlink_insert(Arc::clone(&nlk), 0).expect("netlink_insert failed");
    nlk.inner.lock().flags |= NetlinkFlags::NETLINK_F_KERNEL_SOCKET.bits();

    let mut nl_table = NL_TABLE.write();
    if nl_table[unit].get_registered() == 0 {
        nl_table[unit].set_groups(groups);
        if let Some(cfg) = cfg.as_ref() {
            nl_table[unit].bind = cfg.bind.clone();
            nl_table[unit].unbind = cfg.unbind.clone();
            nl_table[unit].set_flags(cfg.flags);
            if cfg.compare.is_some() {
                nl_table[unit].compare = cfg.compare.clone();
            }
            nl_table[unit].set_registered(1);
        } else {
            drop(listeners);
            let registered = nl_table[unit].get_registered();
            nl_table[unit].set_registered(registered + 1);
        }
    }
    return Ok(nlk);
}

fn __netlink_create(nlk: Arc<NetlinkSock>, unit: usize, kern: usize) -> Result<i32, SystemError> {
    // 获取锁并修改配置参数
    let mut nlk_guard = nlk.inner.lock();
    nlk_guard.flags = kern as u32;
    nlk_guard.protocol = unit;
    Ok(0)
}

pub struct Netlink;

impl family::Family for Netlink {
    /// 用户空间创建一个新的套接字的入口
    fn socket(_stype: Type, _protocol: u32) -> Result<Arc<Inode>, SystemError> {
        let socket = create_netlink_socket(_protocol as usize)?;
        log::debug!("create netlink socket: {:?}", socket);
        Ok(Inode::new(socket))
    }
}
/// 用户空间创建一个新的Netlink套接字
fn create_netlink_socket(_protocol: usize) -> Result<Arc<dyn Socket>, SystemError> {
    match _protocol {
        KOBJECT_UEVENT => Ok(Arc::new(af_netlink::NetlinkSock::new(Some(_protocol)))),
        _ => Err(SystemError::EPROTONOSUPPORT),
    }
}
#[allow(dead_code)]
pub enum SockFlags {
    Dead,
    Done,
    Urginline,
    Keepopen,
    Linger,
    Destroy,
    Broadcast,
    Timestamp,
    Zapped,
    UseWriteQueue,          // whether to call sk->sk_write_space in _wfree
    Dbg,                    // %SO_DEBUG setting
    Rcvtstamp,              // %SO_TIMESTAMP setting
    Rcvtstampns,            // %SO_TIMESTAMPNS setting
    Localroute,             // route locally only, %SO_DONTROUTE setting
    Memalloc,               // VM depends on this et for swapping
    TimestampingRxSoftware, // %SOF_TIMESTAMPING_RX_SOFTWARE
    Fasync,                 // fasync() active
    RxqOvfl,
    Zerocopy,   // buffers from userspace
    WifiStatus, // push wifi status to userspace
    Nofcs,      // Tell NIC not to do the Ethernet FCS.
    // Will use last 4 bytes of packet sent from
    // user-space instead.
    FilterLocked,   // Filter cannot be changed anymore
    SelectErrQueue, // Wake select on error queue
    RcuFree,        // wait rcu grace period in sk_destruct()
    Txtime,
    Xdp,       // XDP is attached
    TstampNew, // Indicates 64 bit timestamps always
    Rcvmark,   // Receive SO_MARK ancillary data with packet
}
