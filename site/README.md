# AgentsPads 产品介绍页

纯 HTML、CSS、JavaScript，无需安装依赖或构建。

在仓库根目录运行：

```powershell
python -m http.server 4175 --bind 127.0.0.1 --directory site
```

访问 [本地预览](http://127.0.0.1:4175/)。使用 `Ctrl + C` 停止。

`index.html` 提供中英文内容，`app.js` 管理文字、快捷键、撤回和指针演示，`base.js` 管理语言、主题和导航，`styles.css` 与 `pads.css` 提供布局及暖色视觉。`assets/logo.png` 直接复用桌面端项目图标。

所有交互都在浏览器内模拟，不连接或控制电脑，不调用麦克风。正式下载链接对应项目 README 的 Windows 安装包、Apple Silicon DMG 和 Android APK。

正式介绍页：[AgentsPads](https://mitsukijoe.github.io/AgentsPads/)。

GitHub Pages 使用 `.github/workflows/pages.yml`，只上传 `site/`。推送介绍页或 Pages 配置的更新到 `main` 后自动部署，也可在 Actions 中手动运行 `Deploy project website`。应用发布工作流排除 `site/**` 与 Pages 配置，单独更新介绍页不会发布软件新版本。
