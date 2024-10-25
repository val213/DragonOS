use super::af_netlink::NetlinkSock;
use crate::libs::{ mutex::Mutex, rwlock::RwLock, spinlock::SpinLock};
use alloc::{sync::Arc, vec::Vec};
const SKB_SIZE: usize = 4096; // 定义 SKB 的大小
#[derive(Debug, Clone)]
pub struct SkBuff {
    // skb的所有者
    pub sk: Arc<SpinLock<NetlinkSock>>,
    pub inner: Arc<Mutex<Vec<Vec<u8>>>>,
}
impl SkBuff {
    pub fn new(protocol: Option<usize>) -> Self {
        SkBuff {
            sk: Arc::new(SpinLock::new(NetlinkSock::new(protocol))),
            inner: Arc::new(Mutex::new(Vec::with_capacity(SKB_SIZE))),
        }
    }
    /// 设置一个网络缓冲区skb的所有者为接收方的套接字sk
    pub fn netlink_skb_set_owner_r(&mut self, sk: Arc<SpinLock<NetlinkSock>>) {
        log::info!("netlink_skb_set_owner_r");
        self.sk = sk;
    }
    /// 对网络套接字(sk)和网络数据包(skb)进行过滤
    pub fn sk_filter(&self, sk: &Arc<SpinLock<NetlinkSock>>) -> bool {
        // todo!()
        false
    }

    // 用于检查网络数据包(skb)是否被共享
    pub fn skb_shared(&self) -> bool {
        // todo!()
        false
    }

    /// 处理被孤儿化的网络数据包(skb)
    /// 孤儿化网络数据包意味着数据包不再与任何套接字关联，
    /// 通常是因为发送数据包时指定了 MSG_DONTWAIT 标志，这告诉内核不要等待必要的资源（如内存），而是尽可能快地发送数据包。
    pub fn skb_orphan(&self) {
        // todo!()
        log::debug!("skb_orphan");
    }

    fn skb_recv_datagram() {}

    fn skb_try_recv_datagram() {}

    fn skb_try_recv_from_queue() {}
}


// 处理网络套接字的缓冲区溢出
pub fn netlink_overrun(sk: &Arc<SpinLock<NetlinkSock>>) {
    // todo!()
}