# Changelog

本文件遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/) 格式，版本号遵循 [语义化版本](https://semver.org/lang/zh-CN/)。

## [Unreleased]

### Fixed

- **点击「立即升级」报 `ubus error 2`，下载从未开始**：rpcd 依据 `args` 中每个提示值的**类型**（而非类型名）推导 blobmsg 策略，`args: { force: 'boolean' }` 中的字符串会落到 `BLOBMSG_TYPE_STRING`，前端传入的 JSON 布尔值因而被 `rpc_ucode_validate_call_args()` 判为类型不符并返回 `UBUS_STATUS_INVALID_ARGUMENT`（2）。改用字面量 `false` 后策略注册为 `Boolean`，`--force` 参数恢复正常。
- **「清空日志」被 ACL 拒绝**：`clear_log` 未写入 `luci-app-online-upgrade` 的 write 授权，LuCI 会话实测 `access:false`，前端调用会得到 `ubus error 6`（Permission denied）。已补齐授权。
