# ShellAliyun

ShellAliyun 是一个用 Rust 编写的阿里云盘终端工具。可以像使用 SFTP 一样交互操作，也可以在脚本中直接调用子命令。

## 安装

从 GitHub Releases 下载与你的系统和架构对应的二进制文件，重命名为 `shellaliyun`，放入 `PATH` 中的目录。Windows 上使用文件名 `shellaliyun.exe`，Linux 和 macOS 上可用 `chmod +x shellaliyun` 授予执行权限。[GitHub Releases](https://github.com/Tardouse/ShellAliyun/releases)

也可以用 Rust 1.87 或更新版本从源码安装。

```bash
git clone https://github.com/Tardouse/ShellAliyun.git
cd ShellAliyun
cargo install --locked --path .
```

## 快速开始

```bash
shellaliyun login
shellaliyun ls --all
shellaliyun
```

在浏览器点击「允许」后，把页面显示的一次性授权码或包含 `code` 的跳转地址粘贴到发起本次登录的终端。授权码有效期为 10 分钟，且只能使用一次。若提示 `InvalidCode`，重新运行 `login`，打开这次生成的链接并复制新的授权码。在 SSH 或无图形界面的环境运行 `shellaliyun login --no-browser`，然后在另一台设备上打开终端显示的链接。

登录凭据默认保存在系统用户配置目录下的 `shellaliyun/credentials.json`。Linux 默认使用 `~/.config` 或 `$XDG_CONFIG_HOME`，macOS 使用 `~/Library/Application Support`，Windows 使用 `%APPDATA%`。Unix 上凭据文件权限为 `0600`。如果只想在当前交互会话使用本次登录，可在 shell 内运行 `login --session-only`。过期后再次运行 `login`。运行 `shellaliyun logout` 可清除保存的登录。

已有访问令牌的脚本可通过 `SHELLALIYUN_TOKEN` 环境变量提供凭据。当前会话内新完成的登录优先，其次是环境变量，再其次是保存的登录。若要撤去环境变量提供的登录，请在调用进程中清除该变量。

## 使用示例

在交互 shell 内可以用 Tab 补全命令、选项、本地路径和当前远程目录中的条目。命令和选项也支持通过 `help`、`help ls` 或 `ls --help` 查看。

```text
aliyun:/root> mkdir /资料
aliyun:/root> cd /资料
aliyun:/root/资料> lcd ~/Downloads
aliyun:/root/资料> put "项目清单.pdf"
aliyun:/root/资料> get "项目清单.pdf" ./备份.pdf
aliyun:/root/资料> search 清单 --return-total
aliyun:/root/资料> quit
```

远程路径以 `/` 开头时从网盘根目录解析，`/root` 也可表示根目录。相对路径支持 `.` 和 `..`。双引号或单引号可包住包含空格的路径。路径以连字符开头时，可以加 `./` 前缀，或在参数前使用 `--`。

直接调用子命令时，远程路径从根目录解析，脚本可以使用退出码判断失败。文件列表写入标准输出，进度、错误和分页提示写入标准错误。

```bash
shellaliyun ls /资料 --all --order-by updated_at --order-direction desc
shellaliyun search 清单 --in /资料 --all
shellaliyun get '/资料/项目清单.pdf' ./备份.pdf
shellaliyun put ./备份.pdf '/资料/项目清单备份.pdf'
```

| 命令 | 用途 |
| --- | --- |
| `ls [目录]` | 列目录。用 `--limit` 控制每页条数，`--marker` 指定起始页，`--all` 自动翻页。 |
| `search <关键词>` | 按名称搜索。默认匹配当前目录的直接子项，支持 `--in <目录>`、`--global`、`--return-total` 和分页。高级表达式使用 `search --query <表达式>`。 |
| `cd <目录>` / `pwd` | 在交互 shell 中切换远程目录，或查看远程目录。 |
| `mkdir <路径>` | 创建远程目录，父目录须已存在。 |
| `lls [目录]` / `lcd <目录>` / `lpwd` | 浏览、切换或查看本地目录。 |
| `put <本地文件> [远程目标]` | 上传单个文件，远程同名时自动重命名。 |
| `get <远程文件> [本地目标]` | 下载单个文件。指定本地目录时沿用远程文件名。传输校验通过后写入目标，使用 `--force` 可替换已有文件。 |
| `cp <源> <目标>` / `mv <源> <目标>` | 复制或移动远程条目。目标为已有目录时保留原名，指定新路径时可重命名。 |
| `rm <路径>` | 移入回收站。使用 `--permanent` 可永久删除。 |
| `exit` / `quit` | 退出交互 shell。 |

下载会校验分片的字节范围和文件大小，并在开放平台返回 SHA-1 时核对哈希值。对于传输中可重试的错误，上传和下载会在本次命令内有限重试。取消后可重新运行命令。

原版操作演示见 [视频](https://github.com/user-attachments/assets/bf751b45-d9b8-4f97-81b9-760375625c93)。

## 开发与验证

```bash
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --locked --release
```

自动化测试使用本地 HTTP 服务模拟阿里云盘，无需账号。GitHub Actions 验证构建并生成 Linux、Windows、macOS 发布产物。推送 `v*` 标签后，通过检查的产物会随 GitHub Release 发布。

项目采用 MIT 许可证。详见 [LICENSE](./LICENSE)。
