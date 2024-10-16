use libc::{sockaddr,  recvfrom, bind, socket, setsockopt, AF_NETLINK, SOCK_DGRAM, getpid, c_void};
use netlink_sys::constants::NETLINK_ADD_MEMBERSHIP;
use nix::libc::{self,  SOL_SOCKET};
use std::fs::File;
use std::io::Write;
use std::os::unix::io::RawFd;
use std::{ mem, io};

#[repr(C)]
struct Nlmsghdr {
    nlmsg_len: u32,
    nlmsg_type: u16,
    nlmsg_flags: u16,
    nlmsg_seq: u32,
    nlmsg_pid: u32,
}

fn create_netlink_socket() -> io::Result<RawFd> {
    let sockfd = unsafe {
        socket(AF_NETLINK, SOCK_DGRAM, libc::NETLINK_KOBJECT_UEVENT)
    };

    if sockfd < 0 {
        println!("Error: {}", io::Error::last_os_error());
        return Err(io::Error::last_os_error());
    }
    Ok(sockfd)
}

fn bind_netlink_socket(sock: RawFd) -> io::Result<()> {
    let pid = unsafe { getpid() };
    let mut addr: libc::sockaddr_nl = unsafe { mem::zeroed() };
    addr.nl_family = AF_NETLINK as u16;
    addr.nl_pid = pid as u32;
    addr.nl_groups = 1;

    let ret = unsafe {
        bind(sock, &addr as *const _ as *const sockaddr, mem::size_of::<libc::sockaddr_nl>() as u32)
    };

    if ret < 0 {
        println!("Error: {}", io::Error::last_os_error());
        return Err(io::Error::last_os_error());
    }

    
    unsafe { setsockopt(sock, SOL_SOCKET,NETLINK_ADD_MEMBERSHIP, &addr.nl_groups as *const _ as *const c_void,
        size_of::<u32>() as libc::socklen_t)};

    println!("bind and setsockopt success");
    Ok(())
}

fn receive_uevent(sock: RawFd) -> io::Result<String> {
    // 检查套接字文件描述符是否有效
    if sock < 0 {
        println!("Invalid socket file descriptor: {}", sock);
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "Invalid socket file descriptor"));
    }

    let mut buf = [0u8; 1024];
    // let mut addr: sockaddr_storage = unsafe { mem::zeroed() };
    // let mut addr_len = mem::size_of::<sockaddr_storage>() as u32;

    // 检查缓冲区指针和长度是否有效
    if buf.is_empty() {
        println!("Buffer is empty");
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "Buffer is empty"));
    }
    let len = unsafe {
        recvfrom(
            sock,
            buf.as_mut_ptr() as *mut c_void,
            buf.len(),
            0,
            core::ptr::null_mut(), // 不接收发送方地址
            core::ptr::null_mut(), // 不接收发送方地址长度
        )
    };
    println!("Received {} bytes", len);
    println!("Received message: {:?}", &buf[..len as usize]);
    if len < 0 {
        println!("Error: {}", io::Error::last_os_error());
        return Err(io::Error::last_os_error());
    }

    let nlmsghdr_size = mem::size_of::<Nlmsghdr>();
    if (len as usize) < nlmsghdr_size {
        println!("Received message is too short");
        return Err(io::Error::new(io::ErrorKind::InvalidData, "Received message is too short"));
    }

    let nlmsghdr = unsafe { &*(buf.as_ptr() as *const Nlmsghdr) };
    if nlmsghdr.nlmsg_len as isize > len {
        println!("Received message is incomplete");
        return Err(io::Error::new(io::ErrorKind::InvalidData, "Received message is incomplete"));
    }

    let message_data = &buf[nlmsghdr_size..nlmsghdr.nlmsg_len as usize];
    Ok(String::from_utf8_lossy(message_data).to_string())
}


// 模拟写入设备的uevent文件，通常在 /sys/class/net/{设备名称}/uevent
fn trigger_device_uevent(device: &str) -> io::Result<()> {
    let uevent_path = format!("/sys/class/net/{}/uevent", device);
    let mut file = File::create(uevent_path)?;
    file.write_all(b"add\n")?;
    println!("Triggered uevent for device {}", device);
    Ok(())
}

fn main() {
    let socket = create_netlink_socket().expect("Failed to create Netlink socket");
    println!("Netlink socket created successfully");

    bind_netlink_socket(socket).expect("Failed to bind Netlink socket");
    println!("Netlink socket created and bound successfully");

    // 向指定网卡设备的 uevent 文件写入事件，模拟设备触发 uevent
    trigger_device_uevent("eth0").expect("Failed to trigger device uevent");
    println!("Device uevent triggered");

    let message = receive_uevent(socket).expect("Failed to receive uevent message");
    println!("Received uevent message: {}", message);
}
