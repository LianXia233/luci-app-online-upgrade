# Changelog

本文件遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/) 格式，版本号遵循 [语义化版本](https://semver.org/lang/zh-CN/)。

## [Unreleased]

### Fixed

- **点击「立即升级」报 `ubus error 2`，下载从未开始**：rpcd 依据 `args` 中每个提示值的**类型**（而非类型名）推导 blobmsg 策略，`args: { force: 'boolean' }` 中的字符串会落到 `BLOBMSG_TYPE_STRING`，前端传入的 JSON 布尔值因而被 `rpc_ucode_validate_call_args()` 判为类型不符并返回 `UBUS_STATUS_INVALID_ARGUMENT`（2）。改用字面量 `false` 后策略注册为 `Boolean`，`--force` 参数恢复正常。
- **「清空日志」被 ACL 拒绝**：`clear_log` 未写入 `luci-app-online-upgrade` 的 write 授权，LuCI 会话实测 `access:false`，前端调用会得到 `ubus error 6`（Permission denied）。已补齐授权。

### Added

- **固件下载线路自动回退**：优先使用所选镜像/直连的 Release 下载地址，失败后自动回退到 GitHub API 资产直链（`api.github.com/repos/<owner>/<repo>/releases/assets/<id>` + `Accept: application/octet-stream` → 签名 CDN）。`github.com` 网页前端返回 5xx / 超时（Release 下载地址由该域名提供）时，升级流程仍可完成。下载请求统一携带 `Accept: application/octet-stream`，该头是 API 资产端点重定向到文件（而非返回 JSON 元数据）的必要条件。
