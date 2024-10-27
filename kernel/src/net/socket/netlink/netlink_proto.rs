// Netlink protocol family
#[allow(dead_code)]
pub mod netlink_protocol {
    pub const ROUTE: usize = 0;
    pub const UNUSED: usize = 1;
    pub const USERSOCK: usize = 2;
    pub const FIREWALL: usize = 3;
    pub const SOCK_DIAG: usize = 4;
    pub const NFLOG: usize = 5;
    pub const XFRM: usize = 6;
    pub const SELINUX: usize = 7;
    pub const ISCSI: usize = 8;
    pub const AUDIT: usize = 9;
    pub const FIB_LOOKUP: usize = 10;
    pub const CONNECTOR: usize = 11;
    pub const NETFILTER: usize = 12;
    pub const IP6_FW: usize = 13;
    pub const DNRTMSG: usize = 14;
    pub const KOBJECT_UEVENT: usize = 15;
    pub const GENERIC: usize = 16;
    pub const SCSITRANSPORT: usize = 18;
    pub const ECRYPTFS: usize = 19;
    pub const RDMA: usize = 20;
    pub const CRYPTO: usize = 21;
    pub const SMC: usize = 22;
    pub const INET_DIAG: usize = 4;
}
