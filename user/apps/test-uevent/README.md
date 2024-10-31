# test_uevent
这是一个测试 uevent 机制的程序，用于测试 uevent 和 Netlink 的部分基础功能。

执行此测试程序，将会执行以下操作：

1.  `create_netlink_socket` 调用 socket 系统调用创建一个 Netlink socket
2. `bind_netlink_socket` 调用 bind 系统调用将 Netlink socket 绑定到 KOBJECT_UEVENT 的 Netlink 协议族和端点
3. 测试 Uevent with Netllink 的消息发送和接收功能，
    - `send_uevent` 测试用户态发送 Netlink socket 到内核：主动发送一个 uevent 消息到 Netlink socket 的缓冲区
4. 测试内核向用户空间发送消息：
    - `trigger_device_uevent` 用户进程主动向指定设备的 uevent 文件写入事件，内核会根据写入的事件字符串生成一个 uevent 事件。内核通过 netlink 套接字将生成的 uevent 消息发送到用户空间。
    - *(todo: 如果实现了 udev，用户空间的 udev 守护进程会监听这些 netlink 消息，获取到 uevent 信息，进而执行相应的动作。)*
5. `receive_uevent` 测试用户态读取 Netlink socket 的消息：主动读取 Netlink socket 的缓冲区中的消息。该程序调用两次 recvmsg 系统调用，第一次读取用户态套接字发送的 uevent 消息，第二次读取内核发送的 uevent 消息。
6. 关闭 Netlink socket