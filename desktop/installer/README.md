# Windows 安装包

正式安装位置可在首次安装时选择。安装器优先选择可用、安全、至少有 256 MB 空间的非 C 盘本地 NTFS 固定磁盘，多个候选按盘符顺序选择，默认目录为 `X:\AgentsPads`；排除系统盘、移动设备、USB/SD/MMC/iSCSI 连接及无法确认连接类型的磁盘。没有合适候选时使用 `%ProgramFiles%\AgentsPads`。用户仍可修改目录；手动选择也必须通过本地固定磁盘及目录保护检查。

开始菜单和桌面统一使用「AgentsPads」入口，直接运行安装目录中的 `agentspads.exe`。应用记住最近一次成功切换的普通或管理员模式，下次从同一入口按所选模式启动。安装包不会保存第二份日常运行程序，也不会在安装结束时从高权限安装器自动启动应用。

更新、重装及手动降级沿用原位置，不重新挑盘。安装位置页会显示原位置并锁定；`/DIR` 也不能绕过检查另装一份。若要换盘，先卸载正式程序再安装到新位置，普通用户数据不会随卸载删除。不会自动迁移磁盘位置或修改整盘、已有父目录的权限。

首次安装的「管理员启动 — 启用操控所有窗口的能力」和「创建桌面快捷方式」都默认不勾选。管理员选项在覆盖安装时也默认不勾选，只有本次明确勾选才改为管理员启动，避免重放旧安装选择、覆盖用户后来切回普通模式的设置。桌面快捷方式选择可以沿用。未勾选时，覆盖安装保留已有模式和授权；全新安装初始化为普通模式，不继承卸载后残留的用户偏好。开机启动由应用设置单独控制，首次安装默认关闭。

应用中的权限开关在成功切换后记住启动模式，切回普通模式仍保留已有管理员授权。登录启动单独记住开启时的权限，不随这个开关改变。普通模式下更新程序或修改登录启动设置需要一次系统授权；默认模式为管理员且已授权时，日常从「AgentsPads」入口启动不重复弹窗。静默管理员启动要求同一管理员账户的普通桌面与提权令牌；使用另一账户的管理员凭据、启用改变账户身份的系统保护时，会明确拒绝这种交接。

## 构建

需要 Windows x64、Rust MSVC 工具链及 [Inno Setup 6.5 或更新版本](https://jrsoftware.org/isdl.php)。在仓库根目录执行：

```powershell
$env:AGENTPAD_VERSION = '1.2.3'
$env:AGENTPAD_BUILD_ID = (git rev-parse HEAD).Trim()
$env:AGENTPAD_CHANNEL = 'stable'
cargo build --manifest-path desktop/Cargo.toml -p agentpad --release --target x86_64-pc-windows-msvc
./desktop/installer/build-windows-installer.ps1 -Version $env:AGENTPAD_VERSION -Executable desktop/target/x86_64-pc-windows-msvc/release/agentpad.exe
```

可以用 `-Compiler` 指定 `ISCC.exe`，用 `-OutputDirectory` 指定产物目录。默认输出 `desktop/installer/dist/agentspads-windows-x64-setup.exe`。脚本会检查 EXE 产品名、资源版本与安装包版本一致，并把载荷的 SHA-256 固定在安装包内，在复制后再次检查。

发布流程先签署 EXE，再用这份 EXE 构建安装包，最后签署安装包。Release 同时保留两个文件：

- `agentspads-windows-x64-setup.exe`：提供给用户安装、手动升级和手动降级。
- `agentspads-windows-x64.exe`：同一次构建的内置更新载荷，不作为另一份日常运行入口。

签名沿用 `WINDOWS_CERTIFICATE_PFX_BASE64` 和 `WINDOWS_CERTIFICATE_PASSWORD`。未配置时产物明确保持未签名；不会宣称已取得 SmartScreen 信誉。CI 的编译器固定为 Inno Setup 6.7.4，下载后核对 SHA-256 和 Authenticode 签名。

## 安装和卸载的约定

唯一安装位置由 64 位 `HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\AgentsPads.Managed.Stable_is1` 下的 `InstallLocation` 登记。安装器在写入和验证正式文件后，将这个产品专属登记键设为管理员所有，关闭权限继承，仅 SYSTEM/Administrators 可写、普通用户可读；保护失败就报告安装未完成。运行入口、计划任务、更新和权限切换都读取该登记位置，不另存一份普通启动路径。

`managed-install.json` 固定内容为 `{"schema":1,"product":"AgentsPads","channel":"stable"}`。只支持正式渠道，不会覆盖其他渠道的安装记录。目录不能使用相对路径、网络盘、SUBST 盘、链接或目录联接。所选目录及关键程序文件不能允许普通程序修改；父目录必须不能被普通程序替换或重定向。安装器只用原子创建方式保护新建的本产品目录，不修订既有目录或整盘权限；不安全的候选会跳过或明确拒绝。因此某台机器的 D 盘不可用时，可能默认选择 F 盘或系统 Program Files。

安装目录不能使用已有的无关非空文件夹。即使选择另一个磁盘，也会检查旧版固定计划任务及原 Program Files 目录，防止绕过旧版清理留下两份活跃程序。

安装器根据覆盖前的有效 `managed-install.json` 和正式 EXE 判断是否已有正式安装，不以用户数据目录是否存在判断。全新安装先运行 `agentspads.exe --configure-normal-launch` 初始化普通模式；失败时停止后续模式配置，明确提示并返回非零退出码。已有正式安装且本次未勾选管理员选项时，不写入启动偏好。

首次复制正式 EXE 前，安装器在受保护安装目录写入 `launch-mode-initialization.pending`。只有普通模式初始化成功且该标记删除成功后，才继续可选的管理员配置；写入或删除失败都会明确报错。安装中断或初始化失败后，修复安装看到标记会继续初始化，正式程序也会提示先修复安装，避免按旧偏好启动。已有完整安装不会新建标记或重设模式。

用户本次勾选管理员启动后，安装器运行 `agentspads.exe --configure-admin-launch`，等待真实退出码。主程序核对当前账户与桌面用户一致，注册授权成功后才保存管理员启动偏好；不同账户的管理员凭据不能代替当前账户授权。配置失败时程序保留，安装器明确提示管理员启动设置未完成并返回非零退出码。

安装器只创建统一的「AgentsPads」快捷方式。安装文件校验、位置登记和旧更新归档成功后，检查原开始菜单中的 `AgentsPads（管理员）.lnk`：只有它仍指向本安装的正式 EXE、参数仍为 `--launch-admin` 时才删除，不删除同名但已被用户改作其他用途的快捷方式。无法检查或删除时明确提示手动清理，不把它静默标作完成。

卸载器在用户确认卸载后、删除程序前运行 `agentspads.exe --unregister-admin-launch`，清理本产品的受管理任务。命令失败则中止卸载，保留程序供修复或重试。卸载不会递归删除用户设置、配对密钥或整个安装目录；它只移除安装器登记的程序文件和快捷方式。

`managed-install.json` 会随保留数据目录留下，便于以后原址重装时识别本产品数据；它不是权威安装位置登记，也不是可运行副本。卸载移除 HKLM 安装登记及正式程序后，这个标记不会使旧目录继续获得管理员启动能力。

主程序持有 `Global\AgentsPads.Managed.Running`，维护操作持有 `Global\AgentsPads.Managed.Maintenance`；安装和卸载同时检查两者，并在实际修改文件前复查。发现运行中的主程序时要求用户从托盘退出；关闭窗口只会隐藏，不算退出。安装器和卸载器持有 `Global\AgentsPads.Managed.Install`，维护期间阻止主程序重新启动和新的更新操作。额外文件锁由 Windows Restart Manager 检测，安装器只请求正常关闭，不强制结束程序、不自动重启，也不会安排重启后替换主程序。文件替换失败或校验失败时必须处理安装器的错误提示，不能把它视为完成了版本切换。

## 失败恢复

覆盖已有正式程序之前，安装器先保存 `agentspads.exe.installer-backup`。如果保存失败，就停止覆盖；已有恢复备份不会被下一次安装覆盖。该文件只用于恢复，不是日常启动入口，也不会注册任务。安装器保留它，直到用户确认新版本可用后手动删除。

安装中断或新版本无法启动时：

1. 退出所有 AgentsPads 进程。
2. 在安装位置（安装器重装页面可查看）检查 `agentspads.exe.installer-backup` 是否存在。
3. 用管理员权限把当前 `agentspads.exe` 改名留存，再将恢复备份复制为同目录的 `agentspads.exe`；已有任务和快捷方式仍指向这个固定位置。
4. 从「AgentsPads」入口启动，检查「关于」中的版本及当前权限。也可以重新运行已知可用版本的安装包。

这份安装备份不会自动判断新版本是否已就绪；应用内更新使用自己的就绪确认及回滚流程。安装完成不等于已经通过首次启动验收。

安装包确认所选程序文件完整后，会把旧的 `updates\transaction.json` 改名为带时间的历史记录，保留恢复文件，避免旧更新事务覆盖用户刚选择的版本。归档失败会明确报告安装未完成。

内置更新下载及验证完成后，使用 Windows 文件替换接口保留原程序，再验证新实例的实际文件哈希、账户、会话、权限、窗口响应和监听状态。确认成功后才提交；失败则恢复原文件与运行权限。更新恢复不替换任务授权或配对密钥。如果系统中断造成正式文件缺失，或文件权限、恢复文件本身损坏，运行已知可用版本的安装包修复；不会假报自动恢复成功。

## 旧开发版清理

没有自动迁移。发现 `run_as_admin.txt`、`ordinary-exe-path.bin` 或没有安装记录的现存 EXE 时，安装器停止并保留原状。旧开发版的拥有者可手动处理：

1. 从托盘退出旧程序。
2. 在任务计划程序中删除 `AgentsPads Elevated` 和 `AgentsPads Elevated Autostart`。
3. 若存在，删除 `%APPDATA%\Microsoft\Windows\Start Menu\Programs\Startup\AgentsPads.bat`。
4. 将 `%ProgramFiles%\AgentsPads` 改名留作备份，然后运行新安装包。普通用户数据无需删除；新管理员身份需要重新扫码配对。

## 验收边界

编译不会安装程序，也不会注册任务。完整 Windows 验收应在隔离测试机/虚拟机内执行：首次安装默认普通模式、残留用户数据不改变新装默认模式、单入口记住成功切换的权限、更新未勾选时保留已有模式、旧管理员快捷方式清理、更新后版本与文件路径一致、手动降级、两种权限下重启、登录自启、授权取消和失败提示、升级/卸载时仍在运行、复制或启动失败后的恢复、卸载任务清理，以及不同账户/会话拒绝串用授权。

`test-install-location.ps1` 可在 Windows PowerShell 5.1 中运行，覆盖目录语法、安全描述符、候选盘选择、既有位置优先及禁止另装一份。它不修改文件、任务或注册表；不能替代高权限实际安装、目录创建及登记保护后的启动验收。`validate-install-root.ps1` 的 `Select`/`Validate` 为只读模式，`Prepare` 创建受保护目录，`ProtectRegistration` 只保护上述产品登记键。

简体中文语言文件来自 [Inno Setup 官方翻译](https://jrsoftware.org/files/istrans/)中 Zhenghan Yang 维护的翻译，保留原作者标识；对应许可见 [LICENSE-Inno-Setup.txt](languages/LICENSE-Inno-Setup.txt)。
