// src/lib/appearance-script.ts

/**
 * Storage keys for appearance preferences.
 * 与 Provider 共享，确保 inline 脚本和 React 层读写同一份数据。
 */
export const STORAGE_KEY_THEME_COLOR = "codeg-theme-color"
export const STORAGE_KEY_ZOOM_LEVEL = "codeg-zoom-level"

// 新会话欢迎页「模式选择区域」（代码开发 / 日常办公 快捷卡片）是否显示。
// 缺省即回退为开启（保持历史行为）；仅在欢迎态客户端渲染，无需预水合。
export const STORAGE_KEY_WELCOME_QUICK_ACTIONS = "codeg-welcome-quick-actions"

// 字体偏好（界面 / 编辑器 / 终端）。预水合阶段不知道浏览器服务器模式，因此不读取
// 这些值；确认 operator/native 后由 AppearanceProvider 恢复。*_FONT 存 id、
// *_CUSTOM 存自定义族名，*_STACK 是已解析的 CSS font-family 栈缓存。
export const STORAGE_KEY_UI_FONT = "codeg-ui-font"
export const STORAGE_KEY_UI_FONT_CUSTOM = "codeg-ui-font-custom"
export const STORAGE_KEY_UI_FONT_STACK = "codeg-ui-font-stack"
export const STORAGE_KEY_EDITOR_FONT = "codeg-editor-font"
export const STORAGE_KEY_EDITOR_FONT_CUSTOM = "codeg-editor-font-custom"
export const STORAGE_KEY_EDITOR_FONT_SIZE = "codeg-editor-font-size"
export const STORAGE_KEY_EDITOR_LIGATURES = "codeg-editor-ligatures"
export const STORAGE_KEY_EDITOR_WORD_WRAP = "codeg-editor-word-wrap"
export const STORAGE_KEY_TERMINAL_FONT = "codeg-terminal-font"
export const STORAGE_KEY_TERMINAL_FONT_CUSTOM = "codeg-terminal-font-custom"
export const STORAGE_KEY_TERMINAL_FONT_SIZE = "codeg-terminal-font-size"
export const STORAGE_KEY_TERMINAL_LIGATURES = "codeg-terminal-ligatures"

// Workspace 背景图片。图片本身存磁盘（~/.codeg/backgrounds/），localStorage 只存
// 展示配置。任何背景状态都等确认 operator/native 后由 AppearanceProvider 恢复；
// 未知/team 模式不能让旧偏好在能力判断前产生样式或网络副作用。
export const STORAGE_KEY_WORKSPACE_BG_ENABLED = "codeg-workspace-bg-enabled"
export const STORAGE_KEY_WORKSPACE_BG_MASK = "codeg-workspace-bg-mask"
export const STORAGE_KEY_WORKSPACE_BG_BLUR = "codeg-workspace-bg-blur"
export const STORAGE_KEY_WORKSPACE_BG_FILL = "codeg-workspace-bg-fill"
export const STORAGE_KEY_WORKSPACE_BG_PANEL_OPACITY =
  "codeg-workspace-bg-panel-opacity"
// 图片存磁盘、写盘无跨窗口信号（外观设置是独立窗口）。用这个版本戳广播失效：
// 写/换/删图后 bump，让 workspace 窗口经 storage 事件重新读盘。不需预水合。
export const STORAGE_KEY_WORKSPACE_BG_IMAGE_VERSION =
  "codeg-workspace-bg-image-version"

// 自定义样式（外观设置页）。全部延迟到确认 operator/native 后再恢复；能力判断前
// 注入自由 CSS 或 URL 的安全风险高于已确认旧模式中一帧的样式变化。
//
// - CUSTOM_THEME：`{ light: {...}, dark: {...} }`，键名不带 `--`（= shadcn
//   registry:theme 的 cssVars 形状，可与 shadcn 生态直接互拷）。
// - CUSTOM_THEME_ENABLED：缺省即开启（空覆盖本就无副作用）。
// - CUSTOM_CSS：已剥离 `@import` 并经 CSSOM 复核的文本，inline 脚本原样注入。
// - CUSTOM_CSS_ENABLED：缺省关闭 —— 自由 CSS 是能把界面改坏的高级功能，必须显式开启。
// - CUSTOM_STYLE_SUSPENDED：逃生舱的持久开关（快捷键触发），置 1 时上面全部不生效。
export const STORAGE_KEY_CUSTOM_THEME = "codeg-custom-theme"
export const STORAGE_KEY_CUSTOM_THEME_ENABLED = "codeg-custom-theme-enabled"
export const STORAGE_KEY_CUSTOM_CSS = "codeg-custom-css"
export const STORAGE_KEY_CUSTOM_CSS_ENABLED = "codeg-custom-css-enabled"
export const STORAGE_KEY_CUSTOM_STYLE_SUSPENDED = "codeg-custom-style-suspended"

/**
 * 同步执行的 inline 脚本，由 layout.tsx 通过 dangerouslySetInnerHTML 注入。
 *
 * 必须在第一帧渲染前完成 <html> 的 data-theme 属性和 font-size 内联样式写入，
 * 否则会出现 FOUC（先看到默认主题/字号，然后切换到用户偏好的闪烁）。
 *
 * 实现要点：
 * 1. 生成出来的是纯字符串，运行时不依赖任何模块导入或外部符号 —— 避免 Next.js
 *    把它当模块编译。构建期的 `${...}` 插值只嵌入允许读取的 storage key。
 * 2. 白名单校验 —— localStorage 里的值若被篡改或残留旧版本，回退到默认
 * 3. try/catch 包裹 —— 隐私模式 / 嵌入 WebView 禁用 storage 时不抛错
 * 4. 数字常量与 theme-presets.ts 保持一致 —— 任何修改必须两边同步
 */
const SCRIPT = `
(function() {
  try {
    var VALID_COLORS = ["neutral","zinc","slate","stone","gray","red","rose","orange","green","blue","yellow","violet"];
    var VALID_ZOOMS = [80, 90, 100, 110, 125, 150, 175, 200, 250, 300];

    var storedColor = localStorage.getItem("${STORAGE_KEY_THEME_COLOR}");
    var color = VALID_COLORS.indexOf(storedColor) >= 0 ? storedColor : "neutral";
    document.documentElement.setAttribute("data-theme", color);

    var storedZoom = parseInt(localStorage.getItem("${STORAGE_KEY_ZOOM_LEVEL}") || "", 10);
    var zoom = VALID_ZOOMS.indexOf(storedZoom) >= 0 ? storedZoom : 100;
    document.documentElement.style.fontSize = (16 * zoom / 100) + "px";

    // 在 next-themes 水合之前同步检测暗色模式，防止白色闪屏。
    // next-themes 使用 localStorage key "theme"，attribute="class"。
    var storedMode = localStorage.getItem("theme");
    var isDark = storedMode === "dark" ||
        (storedMode !== "light" && window.matchMedia("(prefers-color-scheme: dark)").matches);
    if (isDark) {
      document.documentElement.classList.add("dark");
      document.documentElement.style.colorScheme = "dark";
      // 直接设置背景色，比等待 CSS 类匹配更快，覆盖"系统浅色 + 应用深色"场景
      document.documentElement.style.backgroundColor = "#09090b";
    } else {
      document.documentElement.style.colorScheme = "light";
      document.documentElement.style.backgroundColor = "";
    }

    // Deliberately stop at the allowlisted theme, color and zoom subset.
    // Browser capability is unknown at this point, so stored custom CSS,
    // background URLs, fonts and token overrides must wait for the confirmed
    // operator/native AppearanceProvider branch.
  } catch (e) {
    // localStorage 不可用时静默走默认
  }
})();
`

export const APPEARANCE_INIT_SCRIPT = SCRIPT
