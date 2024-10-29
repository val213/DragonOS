# test_uevent
这是一个测试 uevent 机制的应用，用于测试uevent的功能。

执行此测试，将会执行以下操作：

1. 调用 socket 系统调用创建一个 Netlink socket
2. 调用 bind 系统调用将 socket 绑定到 uevent 子系统
3. 测试用户态 socket 的基础读写接口：主动发送一个 uevent 消息到用户态 socket 的缓冲区
4. 测试内核向用户空间发送消息：用户态主动向指定网卡设备的 uevent 文件写入事件，内核会根据写入的事件字符串生成一个 uevent。这个 uevent 包含了事件的详细信息，比如设备名称、事件类型等。内核通过 netlink 套接字将生成的 uevent 发送到用户空间。如果实现了 udev，用户空间的 udev 守护进程会监听这些 netlink 消息，从而获取到 uevent 信息，进而执行相应的动作。