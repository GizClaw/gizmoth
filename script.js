(() => {
  "use strict";

  const site = window.IdySite;
  const repository = "GizClaw/gizmoth";
  const translations = {
    en: {
      pageTitle: "GizMoth — a desktop workspace for GizOS devices",
      pageDescription: "Manage GizOS devices and build Lua apps with an AI coding assistant. Download GizMoth for macOS Apple Silicon, Intel Mac, and Windows x64.",
      features: "Features", downloads: "Downloads",
      starLabel: "Star GizMoth on GitHub (opens in a new tab)",
      heroTitle: "A workspace for your GizOS devices.",
      heroDescription: "Manage your devices and build Lua apps with an AI coding assistant, all from your desktop.",
      getGizmoth: "Download GizMoth", sourceDocs: "Source and documentation",
      platforms: "For macOS Apple Silicon, Intel Mac, and Windows x64.",
      downloadTitle: "Get GizMoth.", downloadDescription: "Choose the installer for your computer.",
      publishedRelease: "Published release", macArm: "macOS · Apple Silicon", macIntel: "macOS · Intel", windows: "Windows · x64",
      allDownloads: "Release notes and all downloads",
      installNote: "See the release notes for installation details.",
      fallbackNote: "Could not refresh the latest version. These installers are from v0.1.0; check GitHub Releases for newer versions.",
      missingNote: "Some installers are unavailable for this release. Check GitHub Releases for available files.",
      featuresTitle: "From pairing to Lua apps.",
      featuresDescription: "Keep your GizOS devices, app projects, and documentation in one desktop workspace.",
      devicesTitle: "Pair and manage devices.", devicesDescription: "Connect supported GizOS devices and manage them from your desktop.",
      appsTitle: "Build Lua apps.", appsDescription: "Create, edit, and package Lua applications in the same workspace.",
      assistantTitle: "Work with an AI assistant.", assistantDescription: "Use a coding assistant alongside offline GizOS documentation while you develop.",
      runTitle: "Send app requests.", runDescription: "Send application run requests to devices or independent simulators.",
      verificationNote: "Device interoperability and end-to-end simulator execution are still being verified.",
    },
    zh: {
      pageTitle: "GizMoth — GizOS 设备的桌面工作空间",
      pageDescription: "管理 GizOS 设备，借助 AI 编程助手开发 Lua 应用。下载适用于 macOS Apple 芯片、Intel Mac 和 Windows x64 的 GizMoth。",
      features: "功能", downloads: "下载",
      starLabel: "在 GitHub 为 GizMoth 点亮 Star（在新标签页打开）",
      heroTitle: "GizOS 设备的桌面工作空间。",
      heroDescription: "在桌面上管理设备，借助 AI 编程助手开发 Lua 应用。",
      getGizmoth: "下载 GizMoth", sourceDocs: "源码和文档",
      platforms: "支持 macOS Apple 芯片、Intel Mac 和 Windows x64。",
      downloadTitle: "下载 GizMoth。", downloadDescription: "选择适合你电脑的安装包。",
      publishedRelease: "已发布版本", macArm: "macOS · Apple 芯片", macIntel: "macOS · Intel", windows: "Windows · x64",
      allDownloads: "发布说明和所有下载",
      installNote: "安装步骤请参阅发布说明。",
      fallbackNote: "暂时无法刷新最新版本。以下为 v0.1.0 安装包，请前往 GitHub Releases 查看更新版本。",
      missingNote: "此版本的部分安装包暂不可用，请前往 GitHub Releases 查看可用文件。",
      featuresTitle: "从设备配对到 Lua 应用。",
      featuresDescription: "在同一个桌面工作空间中管理 GizOS 设备、应用项目和开发文档。",
      devicesTitle: "配对和管理设备。", devicesDescription: "连接支持的 GizOS 设备，在桌面上统一管理。",
      appsTitle: "开发 Lua 应用。", appsDescription: "在同一个工作空间中创建、编辑和打包 Lua 应用。",
      assistantTitle: "与 AI 助手一起开发。", assistantDescription: "使用编程助手，随时查阅离线 GizOS 开发文档。",
      runTitle: "发送应用运行请求。", runDescription: "向设备或独立模拟器发送应用运行请求。",
      verificationNote: "设备互通与模拟器中的应用完整运行仍需进一步验证。",
    },
  };
  const installers = {
    macArm: /^GizMoth_.+_aarch64-apple-darwin\.dmg$/i,
    macIntel: /^GizMoth_.+_x86_64-apple-darwin\.dmg$/i,
    windows: /^GizMoth_.+_x86_64-pc-windows-msvc-setup\.exe$/i,
  };
  let release = null;
  let loading = true;
  let missing = false;

  function render() {
    const stage = document.querySelector("[data-release-stage]");
    const link = document.querySelector("[data-release-link]");
    const status = document.querySelector("[data-release-status]");
    stage.textContent = site.t(release ? (release.prerelease ? "previewRelease" : "stableRelease") : "publishedRelease");
    if (release) {
      link.href = release.html_url;
      link.textContent = release.tag_name;
    }
    status.textContent = site.t(loading ? "loadingRelease" : !release ? "fallbackNote" : missing ? "missingNote" : "installNote");
  }

  document.addEventListener("idy:languagechange", render);
  site.init(translations);
  site.loadRelease(repository)
    .then(value => {
      release = value;
      for (const link of document.querySelectorAll("[data-installer]")) {
        const asset = site.asset(release, installers[link.dataset.installer]);
        link.hidden = !asset;
        if (asset) link.href = asset.browser_download_url;
        else missing = true;
      }
    })
    .catch(() => { release = null; })
    .finally(() => { loading = false; render(); });
})();
