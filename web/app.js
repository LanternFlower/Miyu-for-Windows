(() => {
  "use strict";

  const MAX_CONTENT_CHARS = 20_000;
  const MAX_CUSTOM_ANSWER_CHARS = 4_000;
  const MAX_TOOL_OUTPUT_CHARS = 200_000;
  const MAX_ATTACHMENTS = 12;
  const MAX_ATTACHMENT_BYTES = 10 * 1024 * 1024;
  const MAX_ATTACHMENT_TOTAL_BYTES = 32 * 1024 * 1024;
  const COMMAND_OUTPUT_PREVIEW_ROWS = 8;
  const NEAR_BOTTOM_PX = 120;
  // Mirrors the CSS --ui-scale custom property; mobile drops it to 1 via a
  // media query, so read it at runtime instead of hardcoding.
  let UI_SCALE = 1.1;
  function refreshUiScale() {
    const raw = Number.parseFloat(
      getComputedStyle(document.documentElement).getPropertyValue("--ui-scale")
    );
    if (Number.isFinite(raw) && raw > 0) UI_SCALE = raw;
  }
  refreshUiScale();
  window.addEventListener("resize", refreshUiScale);
  const artifactTextScale = () => 1.2 / UI_SCALE;
  const DEFAULT_BOARD_TITLE = "今天想聊些什么？";
  const DEFAULT_BOARD_SUBTITLE = "从一个问题、计划或此刻的想法开始。";
  const DEFAULT_STARTER_PROMPTS = ["查询今天的天气", "分析一个问题", "发表情包打个招呼吧", "搜索一张图片"];
  const THINKING_VARIANT_LABELS = Object.freeze({
    default: "默认",
    none: "关闭",
    minimal: "最小",
    low: "低",
    medium: "中",
    high: "高",
    xhigh: "极高",
    max: "最大",
    on: "开启",
    off: "关闭",
    auto: "自动"
  });

  function layoutViewportWidth() {
    return (window.innerWidth || document.documentElement.clientWidth || 0) / UI_SCALE;
  }

  function visualPixelsToLayout(value) {
    return Number(value || 0) / UI_SCALE;
  }

  const SVG_NS = "http://www.w3.org/2000/svg";
  const ICONS = {
    "arrow-down": [["path", { d: "M12 5v14" }], ["path", { d: "m19 12-7 7-7-7" }]],
    "arrow-up": [["path", { d: "m5 12 7-7 7 7" }], ["path", { d: "M12 19V5" }]],
    atom: [["circle", { cx: "12", cy: "12", r: "1" }], ["path", { d: "M20.2 20.2c2.04-2.03.02-7.37-4.5-11.9-4.52-4.52-9.87-6.54-11.9-4.5-2.04 2.03-.02 7.37 4.5 11.9 4.52 4.52 9.87 6.54 11.9 4.5Z" }], ["path", { d: "M15.7 15.7c4.52-4.52 6.54-9.87 4.5-11.9-2.03-2.04-7.37-.02-11.9 4.5-4.52 4.52-6.54 9.87-4.5 11.9 2.03 2.04 7.37.02 11.9-4.5Z" }]],
    brain: [["path", { d: "M9.5 4A2.5 2.5 0 0 1 12 6.5v11a2.5 2.5 0 0 1-4.96.44A2.5 2.5 0 0 1 5.5 13a3 3 0 0 1 .34-5.98A2.5 2.5 0 0 1 9.5 4Z" }], ["path", { d: "M14.5 4A2.5 2.5 0 0 0 12 6.5v11a2.5 2.5 0 0 0 4.96.44A2.5 2.5 0 0 0 18.5 13a3 3 0 0 0-.34-5.98A2.5 2.5 0 0 0 14.5 4Z" }]],
    check: [["path", { d: "M20 6 9 17l-5-5" }]],
    "chevron-down": [["path", { d: "m6 9 6 6 6-6" }]],
    terminal: [["polyline", { points: "4 17 10 11 4 5" }], ["line", { x1: "12", x2: "20", y1: "19", y2: "19" }]],
    target: [["circle", { cx: "12", cy: "12", r: "10" }], ["circle", { cx: "12", cy: "12", r: "6" }], ["circle", { cx: "12", cy: "12", r: "2" }]],
    bot: [["path", { d: "M12 8V4H8" }], ["rect", { x: "4", y: "8", width: "16", height: "12", rx: "2" }], ["path", { d: "M2 14h2" }], ["path", { d: "M20 14h2" }], ["path", { d: "M15 13v2" }], ["path", { d: "M9 13v2" }]],
    "book-open": [["path", { d: "M2 3h6a4 4 0 0 1 4 4v14a3 3 0 0 0-3-3H2z" }], ["path", { d: "M22 3h-6a4 4 0 0 0-4 4v14a3 3 0 0 1 3-3h7z" }]],
    image: [["rect", { x: "3", y: "3", width: "18", height: "18", rx: "2", ry: "2" }], ["circle", { cx: "9", cy: "9", r: "2" }], ["path", { d: "m21 15-3.086-3.086a2 2 0 0 0-2.828 0L6 21" }]],
    smile: [["circle", { cx: "12", cy: "12", r: "10" }], ["path", { d: "M8 14s1.5 2 4 2 4-2 4-2" }], ["line", { x1: "9", x2: "9.01", y1: "9", y2: "9" }], ["line", { x1: "15", x2: "15.01", y1: "9", y2: "9" }]],
    "alarm-clock": [["circle", { cx: "12", cy: "13", r: "8" }], ["path", { d: "M12 9v4l2 2" }], ["path", { d: "M5 3 2 6" }], ["path", { d: "m22 6-3-3" }], ["path", { d: "M6.38 18.7 4 21" }], ["path", { d: "M17.64 18.67 20 21" }]],
    clipboard: [["rect", { x: "8", y: "2", width: "8", height: "4", rx: "1", ry: "1" }], ["path", { d: "M16 4h2a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2h2" }]],
    calculator: [["rect", { x: "4", y: "2", width: "16", height: "20", rx: "2" }], ["line", { x1: "8", x2: "16", y1: "6", y2: "6" }], ["line", { x1: "16", x2: "16", y1: "14", y2: "18" }], ["path", { d: "M16 10h.01" }], ["path", { d: "M12 10h.01" }], ["path", { d: "M8 10h.01" }], ["path", { d: "M12 14h.01" }], ["path", { d: "M8 14h.01" }], ["path", { d: "M12 18h.01" }], ["path", { d: "M8 18h.01" }]],
    search: [["circle", { cx: "11", cy: "11", r: "8" }], ["path", { d: "m21 21-4.3-4.3" }]],
    puzzle: [["path", { d: "M19.439 7.85c-.049.322.059.648.289.878l1.568 1.568c.47.47.706 1.087.706 1.704s-.235 1.233-.706 1.704l-1.611 1.611a.98.98 0 0 1-.837.276c-.47-.07-.802-.48-.968-.925a2.501 2.501 0 1 0-3.214 3.214c.446.166.855.497.925.968a.979.979 0 0 1-.276.837l-1.61 1.61a2.404 2.404 0 0 1-1.705.707 2.402 2.402 0 0 1-1.704-.706l-1.568-1.568a1.026 1.026 0 0 0-.877-.29c-.493.074-.84.504-1.02.968a2.5 2.5 0 1 1-3.237-3.237c.464-.18.894-.527.967-1.02a1.026 1.026 0 0 0-.289-.877l-1.568-1.568A2.402 2.402 0 0 1 1.998 12c0-.617.236-1.234.706-1.704L4.23 8.77c.24-.24.581-.353.917-.303.515.077.877.528 1.073 1.01a2.5 2.5 0 1 0 3.259-3.259c-.482-.196-.933-.558-1.01-1.073-.05-.336.062-.676.303-.917l1.525-1.525A2.402 2.402 0 0 1 12 1.998c.617 0 1.234.236 1.704.706l1.568 1.568c.23.23.556.338.877.29.493-.074.84-.504 1.02-.968a2.5 2.5 0 1 1 3.237 3.237c-.464.18-.894.527-.967 1.02Z" }]],
    package: [["path", { d: "m7.5 4.27 9 5.15" }], ["path", { d: "M21 8a2 2 0 0 0-1-1.73l-7-4a2 2 0 0 0-2 0l-7 4A2 2 0 0 0 3 8v8a2 2 0 0 0 1 1.73l7 4a2 2 0 0 0 2 0l7-4A2 2 0 0 0 21 16Z" }], ["path", { d: "m3.3 7 8.7 5 8.7-5" }], ["path", { d: "M12 22V12" }]],
    sparkles: [["path", { d: "M9.937 15.5A2 2 0 0 0 8.5 14.063l-6.135-1.582a.5.5 0 0 1 0-.962L8.5 9.936A2 2 0 0 0 9.937 8.5l1.582-6.135a.5.5 0 0 1 .963 0L14.063 8.5A2 2 0 0 0 15.5 9.937l6.135 1.581a.5.5 0 0 1 0 .964L15.5 14.063a2 2 0 0 0-1.437 1.437l-1.582 6.135a.5.5 0 0 1-.963 0z" }], ["path", { d: "M20 3v4" }], ["path", { d: "M22 5h-4" }]],
    code: [["polyline", { points: "16 18 22 12 16 6" }], ["polyline", { points: "8 6 2 12 8 18" }]],
    arch: [["path", { d: "M12 2c-.9 2.3-1.5 3.8-2.6 5.9.7.7 1.5 1.5 2.8 2.4-1.4-.6-2.4-1.2-3.2-1.8C7.5 11.6 5.2 16 2 22c2.9-1.7 5.4-2.8 7.7-3.3.6-2.1 1.4-3.2 2.3-3.2s1.7 1.1 2.3 3.2c2.3.5 4.8 1.6 7.7 3.3-3.2-6-5.5-10.4-7-13.5-.8.6-1.8 1.2-3.2 1.8 1.3-.9 2.1-1.7 2.8-2.4C13.5 5.8 12.9 4.3 12 2z", fill: "currentColor", stroke: "none" }]],
    "chevron-left": [["path", { d: "m15 18-6-6 6-6" }]],
    "layout-grid": [["rect", { x: "3", y: "3", width: "7", height: "7", rx: "1" }], ["rect", { x: "14", y: "3", width: "7", height: "7", rx: "1" }], ["rect", { x: "14", y: "14", width: "7", height: "7", rx: "1" }], ["rect", { x: "3", y: "14", width: "7", height: "7", rx: "1" }]],
    "chart-column": [["path", { d: "M3 3v16a2 2 0 0 0 2 2h16" }], ["path", { d: "M7 15v-4m5 4V8m5 7v-6" }]],
    "chevron-right": [["path", { d: "m9 18 6-6-6-6" }]],
    "circle-alert": [["circle", { cx: "12", cy: "12", r: "10" }], ["line", { x1: "12", x2: "12", y1: "8", y2: "12" }], ["line", { x1: "12", x2: "12.01", y1: "16", y2: "16" }]],
    "circle-help": [["circle", { cx: "12", cy: "12", r: "10" }], ["path", { d: "M9.09 9a3 3 0 1 1 5.83 1c0 2-3 3-3 3" }], ["path", { d: "M12 17h.01" }]],
    "circle-stop": [["circle", { cx: "12", cy: "12", r: "10" }], ["rect", { width: "6", height: "6", x: "9", y: "9", rx: "1" }]],
    "cloud-sun": [["path", { d: "M12 2v2" }], ["path", { d: "m4.93 4.93 1.41 1.41" }], ["path", { d: "M20 12h2" }], ["path", { d: "m19.07 4.93-1.41 1.41" }], ["path", { d: "M16 6a4 4 0 0 0-3.46 6" }], ["path", { d: "M17.5 19H9a4 4 0 1 1 3.68-5.57A3 3 0 1 1 17.5 19Z" }]],
    copy: [["rect", { width: "14", height: "14", x: "8", y: "8", rx: "2", ry: "2" }], ["path", { d: "M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2" }]],
    "code-2": [["path", { d: "m18 16 4-4-4-4" }], ["path", { d: "m6 8-4 4 4 4" }], ["path", { d: "m14.5 4-5 16" }]],
    download: [["path", { d: "M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" }], ["polyline", { points: "7 10 12 15 17 10" }], ["line", { x1: "12", x2: "12", y1: "15", y2: "3" }]],
    "dollar-sign": [["line", { x1: "12", x2: "12", y1: "2", y2: "22" }], ["path", { d: "M17 5H9.5a3.5 3.5 0 0 0 0 7h5a3.5 3.5 0 0 1 0 7H6" }]],
    ellipsis: [["circle", { cx: "12", cy: "12", r: "1" }], ["circle", { cx: "19", cy: "12", r: "1" }], ["circle", { cx: "5", cy: "12", r: "1" }]],
    eye: [["path", { d: "M2.062 12.348a1 1 0 0 1 0-.696 10.75 10.75 0 0 1 19.876 0 1 1 0 0 1 0 .696 10.75 10.75 0 0 1-19.876 0" }], ["circle", { cx: "12", cy: "12", r: "3" }]],
    "external-link": [["path", { d: "M15 3h6v6" }], ["path", { d: "M10 14 21 3" }], ["path", { d: "M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6" }]],
    folder: [["path", { d: "M3 6a2 2 0 0 1 2-2h5l2 2h7a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2Z" }]],
    globe: [["circle", { cx: "12", cy: "12", r: "10" }], ["path", { d: "M2 12h20" }], ["path", { d: "M12 2a15.3 15.3 0 0 1 0 20" }], ["path", { d: "M12 2a15.3 15.3 0 0 0 0 20" }]],
    "file-text": [["path", { d: "M14.5 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7.5L14.5 2z" }], ["polyline", { points: "14 2 14 8 20 8" }], ["line", { x1: "8", x2: "16", y1: "13", y2: "13" }], ["line", { x1: "8", x2: "16", y1: "17", y2: "17" }]],
    "trash-2": [["path", { d: "M3 6h18" }], ["path", { d: "M8 6V4h8v2" }], ["path", { d: "M19 6 18 20H6L5 6" }], ["path", { d: "M10 11v5" }], ["path", { d: "M14 11v5" }]],
    lightbulb: [["path", { d: "M9 18h6" }], ["path", { d: "M10 22h4" }], ["path", { d: "M15.09 14c.18-.59.59-1.05 1.05-1.52A6 6 0 1 0 7.86 12.5c.45.44.85.9 1.03 1.5" }], ["path", { d: "M9 14h6v1a3 3 0 0 1-6 0v-1Z" }]],
    "list-todo": [["rect", { x: "3", y: "5", width: "6", height: "6", rx: "1" }], ["path", { d: "m3 17 2 2 4-4" }], ["path", { d: "M13 6h8" }], ["path", { d: "M13 12h8" }], ["path", { d: "M13 18h8" }]],
    "loader-circle": [["path", { d: "M21 12a9 9 0 1 1-6.219-8.56" }]],
    "lock-keyhole": [["circle", { cx: "12", cy: "16", r: "1" }], ["rect", { x: "3", y: "10", width: "18", height: "12", rx: "2" }], ["path", { d: "M7 10V7a5 5 0 0 1 10 0v3" }]],
    "log-in": [["path", { d: "M15 3h4a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2h-4" }], ["polyline", { points: "10 17 15 12 10 7" }], ["line", { x1: "15", x2: "3", y1: "12", y2: "12" }]],
    "message-circle": [["path", { d: "M21 15a4 4 0 0 1-4 4H8l-5 3V7a4 4 0 0 1 4-4h10a4 4 0 0 1 4 4z" }]],
    "messages-square": [["path", { d: "M14 9a2 2 0 0 1-2 2H6l-4 4V5a2 2 0 0 1 2-2h8a2 2 0 0 1 2 2z" }], ["path", { d: "M18 9h2a2 2 0 0 1 2 2v10l-4-4h-6a2 2 0 0 1-2-2v-1" }]],
    moon: [["path", { d: "M12 3a6 6 0 0 0 9 9 9 9 0 1 1-9-9Z" }]],
    "image-search": [["rect", { x: "3", y: "3", width: "14", height: "14", rx: "2" }], ["circle", { cx: "11", cy: "9", r: "2" }], ["path", { d: "m3 15 4-4 5 5" }], ["circle", { cx: "18", cy: "18", r: "3" }], ["path", { d: "m20.2 20.2 1.8 1.8" }]],
    image: [["rect", { x: "3", y: "3", width: "18", height: "18", rx: "2" }], ["circle", { cx: "8.5", cy: "8.5", r: "1.5" }], ["path", { d: "m21 15-5-5L5 21" }]],
    "file-code": [["path", { d: "M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z" }], ["path", { d: "M14 2v6h6" }], ["path", { d: "m10 13-2 2 2 2" }], ["path", { d: "m14 13 2 2-2 2" }]],
    "file-markdown": [["path", { d: "M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z" }], ["path", { d: "M14 2v6h6" }], ["path", { d: "M8 16v-4l2 2 2-2v4" }], ["path", { d: "M15 12v4" }]],
    "file-json": [["path", { d: "M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z" }], ["path", { d: "M14 2v6h6" }], ["path", { d: "M8 12h1a1 1 0 0 1 0 2H8v2h1a1 1 0 0 1 0 2H8" }], ["path", { d: "M16 12h-1a1 1 0 0 0 0 2h1v2h-1" }]],
    "maximize-2": [["path", { d: "M15 3h6v6" }], ["path", { d: "m21 3-7 7" }], ["path", { d: "m3 21 7-7" }], ["path", { d: "M9 21H3v-6" }]],
    "minimize-2": [["path", { d: "m14 10 7-7" }], ["path", { d: "M20 10h-6V4" }], ["path", { d: "m3 21 7-7" }], ["path", { d: "M4 14h6v6" }]],
    paintbrush: [["path", { d: "m14.622 17.897-10.68-2.913" }], ["path", { d: "M18.376 2.622a1 1 0 0 1 3.002 3.002L17.36 9.642a2 2 0 0 1-2.121.447l-2.741-1.02a1 1 0 0 1-.583-.583l-1.02-2.741a2 2 0 0 1 .447-2.121Z" }], ["path", { d: "M9 8c-1.804.716-3.5 2.5-3.5 4.5 0 .6.4 1 1 1 2 0 3.784-1.696 4.5-3.5" }]],
    "panel-left": [["rect", { width: "18", height: "18", x: "3", y: "3", rx: "2" }], ["path", { d: "M9 3v18" }]],
    "panel-left-close": [["rect", { width: "18", height: "18", x: "3", y: "3", rx: "2" }], ["path", { d: "M9 3v18" }], ["path", { d: "m15 9-3 3 3 3" }]],
    "panel-left-open": [["rect", { width: "18", height: "18", x: "3", y: "3", rx: "2" }], ["path", { d: "M9 3v18" }], ["path", { d: "m12 9 3 3-3 3" }]],
    "panel-right": [["rect", { width: "18", height: "18", x: "3", y: "3", rx: "2" }], ["path", { d: "M15 3v18" }]],
    paperclip: [["path", { d: "m21.44 11.05-9.19 9.19a6 6 0 0 1-8.49-8.49l9.19-9.19a4 4 0 0 1 5.66 5.66l-9.2 9.19a2 2 0 0 1-2.83-2.83l8.49-8.48" }]],
    "refresh-cw": [["path", { d: "M21 12a9 9 0 0 0-15.35-6.35L3 8" }], ["path", { d: "M3 3v5h5" }], ["path", { d: "M3 12a9 9 0 0 0 15.35 6.35L21 16" }], ["path", { d: "M16 16h5v5" }]],
    route: [["circle", { cx: "6", cy: "19", r: "3" }], ["path", { d: "M9 19h8.5a3.5 3.5 0 0 0 0-7h-11a3.5 3.5 0 0 1 0-7H15" }], ["circle", { cx: "18", cy: "5", r: "3" }]],
    "settings-2": [["path", { d: "M20 7h-9" }], ["path", { d: "M14 17H5" }], ["circle", { cx: "17", cy: "17", r: "3" }], ["circle", { cx: "7", cy: "7", r: "3" }]],
    "sliders-horizontal": [["line", { x1: "21", x2: "14", y1: "4", y2: "4" }], ["line", { x1: "10", x2: "3", y1: "4", y2: "4" }], ["line", { x1: "21", x2: "12", y1: "12", y2: "12" }], ["line", { x1: "8", x2: "3", y1: "12", y2: "12" }], ["line", { x1: "21", x2: "16", y1: "20", y2: "20" }], ["line", { x1: "12", x2: "3", y1: "20", y2: "20" }], ["line", { x1: "14", x2: "14", y1: "2", y2: "6" }], ["line", { x1: "8", x2: "8", y1: "10", y2: "14" }], ["line", { x1: "16", x2: "16", y1: "18", y2: "22" }]],
    sparkles: [["path", { d: "m12 3-1.9 5.8a2 2 0 0 1-1.3 1.3L3 12l5.8 1.9a2 2 0 0 1 1.3 1.3L12 21l1.9-5.8a2 2 0 0 1 1.3-1.3L21 12l-5.8-1.9a2 2 0 0 1-1.3-1.3Z" }], ["path", { d: "M5 3v4" }], ["path", { d: "M19 17v4" }], ["path", { d: "M3 5h4" }], ["path", { d: "M17 19h4" }]],
    smile: [["circle", { cx: "12", cy: "12", r: "9" }], ["path", { d: "M8 14s1.5 2 4 2 4-2 4-2" }], ["path", { d: "M9 9h.01" }], ["path", { d: "M15 9h.01" }]],
    "stop-square": [["rect", { x: "6", y: "6", width: "12", height: "12", rx: "2", fill: "currentColor", stroke: "none" }]],
    "square-pen": [["path", { d: "M12 3H5a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2v-7" }], ["path", { d: "M18.37 2.63a2.121 2.121 0 0 1 3 3L12 15l-4 1 1-4Z" }]],
    sun: [["circle", { cx: "12", cy: "12", r: "4" }], ["path", { d: "M12 2v2" }], ["path", { d: "M12 20v2" }], ["path", { d: "m4.93 4.93 1.42 1.42" }], ["path", { d: "m17.66 17.66 1.41 1.41" }], ["path", { d: "M2 12h2" }], ["path", { d: "M20 12h2" }], ["path", { d: "m6.34 17.66-1.41 1.41" }], ["path", { d: "m19.07 4.93-1.41 1.41" }]],
    "sun-moon": [["path", { d: "M12 8a2.83 2.83 0 0 0 4 4 4 4 0 1 1-4-4" }], ["path", { d: "M12 2v2" }], ["path", { d: "M12 20v2" }], ["path", { d: "m4.9 4.9 1.4 1.4" }], ["path", { d: "m17.7 17.7 1.4 1.4" }], ["path", { d: "M2 12h2" }], ["path", { d: "M20 12h2" }], ["path", { d: "m6.3 17.7-1.4 1.4" }], ["path", { d: "m19.1 4.9-1.4 1.4" }]],
    "triangle-alert": [["path", { d: "m21.73 18-8-14a2 2 0 0 0-3.46 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3" }], ["path", { d: "M12 9v4" }], ["path", { d: "M12 17h.01" }]],
    wrench: [["path", { d: "M14.7 6.3a1 1 0 0 0 0 1.4l1.6 1.6a1 1 0 0 0 1.4 0l3.77-3.77a6 6 0 0 1-7.94 7.94l-6.91 6.91a2.12 2.12 0 0 1-3-3l6.91-6.91a6 6 0 0 1 7.94-7.94z" }]],
    "zoom-in": [["circle", { cx: "11", cy: "11", r: "8" }], ["path", { d: "m21 21-4.3-4.3" }], ["path", { d: "M11 8v6" }], ["path", { d: "M8 11h6" }]],
    "zoom-out": [["circle", { cx: "11", cy: "11", r: "8" }], ["path", { d: "m21 21-4.3-4.3" }], ["path", { d: "M8 11h6" }]],
    x: [["path", { d: "M18 6 6 18" }], ["path", { d: "m6 6 12 12" }]]
  };

  const EVENT_NAMES = [
    "run.started",
    "turn.started",
    "assistant.delta",
    "reasoning.start",
    "reasoning.reset",
    "reasoning.part_start",
    "reasoning.part_end",
    "reasoning.title",
    "reasoning.delta",
    "tool.started",
    "tool.preparing",
    "tool.progress",
    "tool.output",
    "tool.image",
    "tool.artifact",
    "tool.finished",
    "question.requested",
    "question.answered",
    "question.closed",
    "context.compact_start",
    "context.compact_delta",
    "context.compact_end",
    "context.pop_start",
    "context.pop_end",
    "context.error",
    "queue.added",
    "queue.removed",
    "queue.consumed",
    "generation.superseded",
    "chat.round_usage",
    "run.completed",
    "run.cancelled",
    "run.failed",
    "conversation.reset",
    "conversation.pop",
    "session.created",
    "session.renamed",
    "session.deleted",
    "session.current_changed",
    "session.updated",
    "job.started",
    "job.finished",
    "job.acknowledged",
    "resync_required"
  ];

  const RUN_EVENTS = new Set(EVENT_NAMES.filter((name) => !name.startsWith("session.") && !name.startsWith("job.") && !["conversation.reset", "conversation.pop", "resync_required", "queue.added", "queue.removed"].includes(name)));

  const elements = {
    body: document.body,
    appShell: document.getElementById("appShell"),
    mainStage: document.getElementById("mainStage"),
    sidebar: document.getElementById("sidebar"),
    sidebarScrim: document.getElementById("sidebarScrim"),
    sidebarClose: document.getElementById("sidebarClose"),
    sidebarCollapseButton: document.getElementById("sidebarCollapseButton"),
    sidebarExpandButton: document.getElementById("sidebarExpandButton"),
    mobileMenuButton: document.getElementById("mobileMenuButton"),
    sidebarStatusDot: document.getElementById("sidebarStatusDot"),
    sidebarConnectionStatus: document.getElementById("sidebarConnectionStatus"),
    newChatButton: document.getElementById("newChatButton"),
    matugenThemeLink: document.getElementById("matugenThemeLink"),
    reasoningExpandToggle: document.getElementById("reasoningExpandToggle"),
    toolExpandToggle: document.getElementById("toolExpandToggle"),
    sessionList: document.getElementById("sessionList"),
    sessionItems: document.getElementById("sessionItems"),
    contextNumbers: document.getElementById("contextNumbers"),
    contextTrack: document.getElementById("contextTrack"),
    contextBar: document.getElementById("contextBar"),
    consoleButton: document.getElementById("consoleButton"),
    consoleView: document.getElementById("consoleView"),
    consoleBack: document.getElementById("consoleBack"),
    conRailToggle: document.getElementById("conRailToggle"),
    usageStamp: document.getElementById("usageStamp"),
    usageRangeSeg: document.getElementById("usageRangeSeg"),
    usageTiles: document.getElementById("usageTiles"),
    usageHeatmap: document.getElementById("usageHeatmap"),
    usageHeatMonths: document.getElementById("usageHeatMonths"),
    usageHeatTotal: document.getElementById("usageHeatTotal"),
    usageBars: document.getElementById("usageBars"),
    usageBarsX: document.getElementById("usageBarsX"),
    usageBarsY: document.getElementById("usageBarsY"),
    usageBarsHint: document.getElementById("usageBarsHint"),
    usageSources: document.getElementById("usageSources"),
    usageRecords: document.getElementById("usageRecords"),
    usageRefresh: document.getElementById("usageRefresh"),
    usageSrcFilter: document.getElementById("usageSrcFilter"),
    usageModelFilter: document.getElementById("usageModelFilter"),
    sidebarThemeButton: document.getElementById("sidebarThemeButton"),
    brandAvatar: document.getElementById("brandAvatar"),
    brandName: document.getElementById("brandName"),
    conversationTitle: document.getElementById("conversationTitle"),
    conversationMeta: document.getElementById("conversationMeta"),
    modeBadge: document.getElementById("modeBadge"),
    thinkingVariantButton: document.getElementById("thinkingVariantButton"),
    thinkingVariantPopover: document.getElementById("thinkingVariantPopover"),
    thinkingModelList: document.getElementById("thinkingModelList"),
    thinkingLevelList: document.getElementById("thinkingLevelList"),
    thinkingModelName: document.getElementById("thinkingModelName"),
    thinkingModelProvider: document.getElementById("thinkingModelProvider"),
    modelMenuWrap: document.getElementById("modelMenuWrap"),
    modelButton: document.getElementById("modelButton"),
    modelMark: document.getElementById("modelMark"),
    modelLabel: document.getElementById("modelLabel"),
    modelMenu: document.getElementById("modelMenu"),
    themeButton: document.getElementById("themeButton"),
    topbarSettingsButton: document.getElementById("topbarSettingsButton"),
    artifactToggleButton: document.getElementById("artifactToggleButton"),
    artifactWorkspace: document.getElementById("artifactWorkspace"),
    artifactResizeHandle: document.getElementById("artifactResizeHandle"),
    artifactCloseButton: document.getElementById("artifactCloseButton"),
    artifactTitle: document.getElementById("artifactTitle"),
    artifactTypeLabel: document.getElementById("artifactTypeLabel"),
    artifactTitleButton: document.getElementById("artifactTitleButton"),
    artifactResourceMenu: document.getElementById("artifactResourceMenu"),
    artifactPreviewButton: document.getElementById("artifactPreviewButton"),
    artifactSourceButton: document.getElementById("artifactSourceButton"),
    artifactImageActions: document.getElementById("artifactImageActions"),
    artifactImageExternalButton: document.getElementById("artifactImageExternalButton"),
    artifactImageZoomOutButton: document.getElementById("artifactImageZoomOutButton"),
    artifactImageZoomInButton: document.getElementById("artifactImageZoomInButton"),
    artifactCopyButton: document.getElementById("artifactCopyButton"),
    artifactDownloadButton: document.getElementById("artifactDownloadButton"),
    artifactMaximizeButton: document.getElementById("artifactMaximizeButton"),
    artifactView: document.getElementById("artifactView"),
    errorRegion: document.getElementById("errorRegion"),
    chatScroll: document.getElementById("chatScroll"),
    loadingState: document.getElementById("loadingState"),
    blockedState: document.getElementById("blockedState"),
    blockedTitle: document.getElementById("blockedTitle"),
    blockedMessage: document.getElementById("blockedMessage"),
    loginForm: document.getElementById("loginForm"),
    loginPassword: document.getElementById("loginPassword"),
    loginError: document.getElementById("loginError"),
    loginSubmit: document.getElementById("loginSubmit"),
    loginSubmitLabel: document.getElementById("loginSubmitLabel"),
    retryBootstrapButton: document.getElementById("retryBootstrapButton"),
    timeline: document.getElementById("timeline"),
    emptyState: document.getElementById("emptyState"),
    emptyVisual: document.getElementById("emptyVisual"),
    emptyBoardImage: document.getElementById("emptyBoardImage"),
    emptyKickerName: document.getElementById("emptyKickerName"),
    emptyTitle: document.getElementById("emptyTitle"),
    emptySubtitle: document.getElementById("emptySubtitle"),
    promptGrid: document.getElementById("promptGrid"),
    jumpBottomButton: document.getElementById("jumpBottomButton"),
    composerDock: document.getElementById("composerDock"),
    jobsStrip: document.getElementById("jobsStrip"),
    liveStopRail: document.getElementById("liveStopRail"),
    questionDock: document.getElementById("questionDock"),
    composerForm: document.getElementById("composerForm"),
    composerInput: document.getElementById("composerInput"),
    attachmentTray: document.getElementById("attachmentTray"),
    attachmentInput: document.getElementById("attachmentInput"),
    attachButton: document.getElementById("attachButton"),
    queueTray: document.getElementById("queueTray"),
    composerState: document.getElementById("composerState"),
    characterCount: document.getElementById("characterCount"),
    sendButton: document.getElementById("sendButton"),
    drawerScrim: document.getElementById("drawerScrim"),
    settingsDrawer: document.getElementById("settingsDrawer"),
    settingsClose: document.getElementById("settingsClose"),
    settingsNav: document.querySelector(".settings-nav"),
    settingsPanels: Array.from(document.querySelectorAll("[data-settings-panel]")),
    settingsModelMark: document.getElementById("settingsModelMark"),
    settingsModelName: document.getElementById("settingsModelName"),
    settingsModelProvider: document.getElementById("settingsModelProvider"),
    capabilityList: document.getElementById("capabilityList"),
    versionLabel: document.getElementById("versionLabel"),
    generalConfigForm: document.getElementById("generalConfigForm"),
    providerEditor: document.getElementById("providerEditor"),
    addProviderButton: document.getElementById("addProviderButton"),
    modelPoolEditor: document.getElementById("modelPoolEditor"),
    pluginEditor: document.getElementById("pluginEditor"),
    promptEditor: document.getElementById("promptEditor"),
    advancedConfigEditor: document.getElementById("advancedConfigEditor"),
    qqHistoryForm: document.getElementById("qqHistoryForm"),
    qqHistoryAccount: document.getElementById("qqHistoryAccount"),
    qqHistoryGroup: document.getElementById("qqHistoryGroup"),
    qqHistoryStatus: document.getElementById("qqHistoryStatus"),
    qqHistoryOutput: document.getElementById("qqHistoryOutput"),
    applyAdvancedConfigButton: document.getElementById("applyAdvancedConfigButton"),
    reloadConfigButton: document.getElementById("reloadConfigButton"),
    saveConfigButton: document.getElementById("saveConfigButton"),
    settingsStatus: document.getElementById("settingsStatus"),
    toastRegion: document.getElementById("toastRegion"),
    resetDialog: document.getElementById("resetDialog"),
    resetCancelButton: document.getElementById("resetCancelButton"),
    resetConfirmButton: document.getElementById("resetConfirmButton")
  };

  const state = {
    backgroundJobs: new Map(),
    jobsStripOpen: localStorage.getItem("miyu.web.jobsStripOpen") === "1",
    bootId: null,
    latestEventId: 0,
    lastEventId: 0,
    replayRunIds: null,
    replayCutoff: 0,
    replayResyncCount: 0,
    replayResyncAt: 0,
    turns: [],
    queuedPrompts: [],
    models: [],
    persona: {
      name: "Miyu",
      avatar_url: "/assets/miyu-logo.png",
      board_image_url: "/assets/miyuwallpaper.png",
      board_title: DEFAULT_BOARD_TITLE,
      board_subtitle: DEFAULT_BOARD_SUBTITLE,
      starter_prompts: DEFAULT_STARTER_PROMPTS
    },
    sessions: [],
    currentSessionId: null,
    viewSessionId: null,
    viewRunningTurnId: null,
    viewLoading: false,
    viewLoadGeneration: 0,
    viewSyncTimer: null,
    runsBySession: new Map(),
    liveRuns: new Map(),
    sessionMenuFor: null,
    sessionRenaming: null,
    modeChooserOpen: false,
    modeChooserKeyHandler: null,
    sessionBusy: false,
    display: {
      reasoning: "summary",
      tool_calls: "summary",
      readable_tool_names: true,
      command_output_lines: 10,
      mixed_model_endpoint_display: "interactive",
      show_mixed_model_endpoint: false
    },
    context: { tokens: 0, window: null },
    usage: {},
    capabilities: {},
    version: null,
    eventSource: null,
    connection: "connecting",
    blocked: false,
    adminBusy: false,
    loginSubmitting: false,
    modelSelectionSubmitting: false,
    stagedModelKeys: null,
    stagedFollowGlobal: false,
    modelMenuTouched: false,
    modelMenuError: "",
    sessionModelOverride: null,
    sessionModelOverrideFor: "",
    sessionModelOverrideToken: 0,
    submitting: false,
    revisionSubmitting: false,
    redoCandidate: null,
    revisionEditor: null,
    pendingSubmission: null,
    composerAttachments: [],
    artifacts: [],
    selectedArtifactId: null,
    artifactOpen: false,
    artifactRenderToken: 0,
    artifactZoom: 1,
    artifactPanX: 0,
    artifactPanY: 0,
    artifactMode: "preview",
    artifactMaximized: false,
    artifactWidthRatio: 0.5,
    artifactSourceCache: new Map(),
    colorScheme: null,
    matugenAvailable: null,
    reasoningExpanded: false,
    toolExpanded: false,
    finishedTurnArticles: new Map(),
    bootstrapPromise: null,
    resyncing: false,
    nearBottom: true,
    followOutput: true,
    scrollRequestId: 0,
    programmaticScroll: false,
    settingsOpener: null,
    sidebarOpener: null,
    sidebarCollapsed: false,
    sidebarAutoCollapsed: false,
    toastTimer: null,
    modeAnimationTimer: null,
    healthTimer: null,
    terminalRunIds: new Set(),
    thinkingVariantModels: [],
    thinkingVariantActiveKey: null,
    thinkingVariantLoading: false,
    thinkingVariantLoadGeneration: 0,
    thinkingVariantError: "",
    thinkingVariantConfirmed: new Map(),
    thinkingVariantRevisions: new Map(),
    thinkingVariantWriteChain: Promise.resolve(),
    composing: false,
    settingsView: "interface",
    configLoaded: false,
    configLoading: false,
    configSaving: false,
    configDirty: false,
    configDraft: null,
    configOriginal: null,
    promptDraft: null,
    promptOriginal: null,
    secretStates: {},
    secretChanges: {},
    providerSecretStates: [],
    configMultimodalModels: [],
    configInferredImageModels: [],
    invalidConfigFields: new Map()
  };

  class ApiError extends Error {
    constructor(message, status) {
      super(message);
      this.name = "ApiError";
      this.status = status;
    }
  }

  function createIcon(name, className = "") {
    const svg = document.createElementNS(SVG_NS, "svg");
    svg.setAttribute("viewBox", "0 0 24 24");
    svg.setAttribute("fill", "none");
    svg.setAttribute("stroke", "currentColor");
    svg.setAttribute("stroke-width", "2");
    svg.setAttribute("stroke-linecap", "round");
    svg.setAttribute("stroke-linejoin", "round");
    svg.setAttribute("aria-hidden", "true");
    svg.setAttribute("focusable", "false");
    if (className) svg.setAttribute("class", className);
    const definition = ICONS[name] || ICONS["circle-alert"];
    for (const [tag, attributes] of definition) {
      const node = document.createElementNS(SVG_NS, tag);
      for (const [key, value] of Object.entries(attributes)) node.setAttribute(key, value);
      svg.appendChild(node);
    }
    return svg;
  }

  function renderIconSlots(root = document) {
    const slots = [];
    if (root instanceof Element && root.matches("[data-icon]")) slots.push(root);
    slots.push(...root.querySelectorAll("[data-icon]"));
    for (const slot of slots) {
      slot.replaceChildren(createIcon(slot.dataset.icon));
    }
  }

  function makeIconSlot(name, className = "") {
    const slot = document.createElement("span");
    slot.className = `icon-slot${className ? ` ${className}` : ""}`;
    slot.setAttribute("aria-hidden", "true");
    slot.appendChild(createIcon(name));
    return slot;
  }

  function safeStorageGet(key) {
    try {
      return window.localStorage.getItem(key);
    } catch (_) {
      return null;
    }
  }

  function safeStorageSet(key, value) {
    try {
      window.localStorage.setItem(key, value);
    } catch (_) {
      // Storage can be unavailable in hardened browser profiles.
    }
  }

  function setTheme(theme, persist = true) {
    const selected = theme === "linen" ? "linen" : "graphite";
    elements.body.dataset.theme = selected;
    document.querySelectorAll("[data-theme-choice]").forEach((button) => {
      button.classList.toggle("selected", button.dataset.themeChoice === selected);
      button.setAttribute("aria-pressed", String(button.dataset.themeChoice === selected));
    });
    const nextIcon = selected === "graphite" ? "sun" : "moon";
    for (const button of [elements.themeButton, elements.sidebarThemeButton]) {
      const slot = button.querySelector(".icon-slot");
      slot.replaceChildren(createIcon(nextIcon));
      button.title = selected === "graphite" ? "切换到晨光主题" : "切换到夜阑主题";
      button.setAttribute("aria-label", button.title);
    }
    const themeColor = document.querySelector('meta[name="theme-color"]');
    if (themeColor) themeColor.content = selected === "graphite" ? "#171821" : "#f6f0e2";
    if (persist) safeStorageSet("miyu.web.theme", selected);
  }

  /*
   * 配色方案(与明暗正交):
   * - madobe  窗边预设(logo 派生 token,styles.css 内置)
   * - matugen 壁纸取色(后端 /theme.css 输出整套 MD3 token)
   * 通过禁用 /theme.css 的 <link> 切换,不改后端与 matugen 模板。
   */
  function setColorScheme(scheme, persist = true) {
    const requested = scheme === "madobe" ? "madobe" : "matugen";
    const selected = requested === "matugen" && state.matugenAvailable === false ? "madobe" : requested;
    state.colorScheme = selected;
    elements.body.dataset.colorScheme = selected;
    if (elements.matugenThemeLink) elements.matugenThemeLink.disabled = selected !== "matugen";
    document.querySelectorAll("[data-scheme-choice]").forEach((button) => {
      const active = button.dataset.schemeChoice === selected;
      button.classList.toggle("selected", active);
      button.setAttribute("aria-pressed", String(active));
      // 探测不到 matugen 输出时,「壁纸取色」整个选项不显示。
      if (button.dataset.schemeChoice === "matugen") button.hidden = state.matugenAvailable !== true;
    });
    if (persist) safeStorageSet("miyu.web.colorScheme", requested);
  }

  async function probeMatugenTheme() {
    try {
      const response = await fetch("/theme.css", { method: "HEAD", cache: "no-store" });
      state.matugenAvailable = response.ok;
    } catch (_) {
      state.matugenAvailable = false;
    }
    // 无持久化记录时:matugen 可用则维持现状(matugen),否则窗边。默认值不写入存储。
    setColorScheme(safeStorageGet("miyu.web.colorScheme") || (state.matugenAvailable ? "matugen" : "madobe"), false);
  }

  /* 仅 WebUI 的本地显示偏好(localStorage,不写入 config) */
  const CHAT_FONT_SIZES = ["14px", "15px", "16px"];

  function setChatFontSize(size, persist = true) {
    const selected = CHAT_FONT_SIZES.includes(size) ? size : "15px";
    document.documentElement.style.setProperty("--fs-chat", selected);
    document.documentElement.style.setProperty("--fs-artifact-chat", `${Number.parseFloat(selected) * artifactTextScale()}px`);
    document.querySelectorAll("[data-chat-font]").forEach((button) => {
      const active = button.dataset.chatFont === selected;
      button.classList.toggle("active", active);
      button.setAttribute("aria-pressed", String(active));
    });
    if (persist) safeStorageSet("miyu.web.chatFontSize", selected);
  }

  function setReasoningExpanded(value, persist = true) {
    state.reasoningExpanded = Boolean(value);
    elements.reasoningExpandToggle?.setAttribute("aria-checked", String(state.reasoningExpanded));
    // 对已渲染的思考块即时生效
    document.querySelectorAll(".reasoning-block").forEach((block) => {
      block.open = state.reasoningExpanded;
    });
    if (persist) safeStorageSet("miyu.web.reasoningExpanded", String(state.reasoningExpanded));
  }

  function setToolExpanded(value, persist = true) {
    state.toolExpanded = Boolean(value);
    elements.toolExpandToggle?.setAttribute("aria-checked", String(state.toolExpanded));
    // 对已渲染的工具签即时生效
    document.querySelectorAll(".tool-card").forEach((card) => {
      card.classList.toggle("collapsed", !state.toolExpanded);
      card.querySelector(".tool-head")?.setAttribute("aria-expanded", String(state.toolExpanded));
    });
    if (persist) safeStorageSet("miyu.web.toolExpanded", String(state.toolExpanded));
  }

  function thinkingVariantLabel(variant, short = false) {
    if (variant == null) return short ? "默认" : "模型默认";
    const value = String(variant);
    return THINKING_VARIANT_LABELS[value.toLowerCase()] || value;
  }

  function normalizeThinkingVariantModels(value) {
    if (!Array.isArray(value)) return [];
    return value.flatMap((item) => {
      const providerId = String(item?.provider_id || "").trim();
      const model = String(item?.model || "").trim();
      if (!providerId || !model) return [];
      const variants = Array.from(new Set(
        (Array.isArray(item?.variants) ? item.variants : [])
          .map((variant) => String(variant).trim())
          .filter(Boolean)
      ));
      const selected = typeof item?.selected === "string" && variants.includes(item.selected)
        ? item.selected
        : null;
      return [{ provider_id: providerId, model, variants, selected }];
    });
  }

  function thinkingVariantModel(key = state.thinkingVariantActiveKey) {
    return state.thinkingVariantModels.find((model) => modelKey(model) === key) || null;
  }

  function thinkingVariantDisplay(model) {
    const configured = state.models.find((candidate) => modelKey(candidate) === modelKey(model));
    return {
      name: String(configured?.model || model?.model || ""),
      provider: String(configured?.provider_name || configured?.provider_id || model?.provider_id || "")
    };
  }

  function updateThinkingVariantTrigger() {
    const models = state.thinkingVariantModels;
    const hasOverride = models.some((model) => model.selected != null);
    elements.thinkingVariantButton.classList.toggle("has-override", hasOverride);
    if (state.thinkingVariantError && models.length === 0) {
      elements.thinkingVariantButton.title = state.thinkingVariantError;
      elements.thinkingVariantButton.setAttribute("aria-label", `思考程度暂不可用：${state.thinkingVariantError}`);
      return;
    }
    const summary = models.length === 1
      ? thinkingVariantLabel(models[0].selected)
      : `${models.length} 个模型`;
    elements.thinkingVariantButton.title = models.length ? `思考程度：${summary}` : "当前模型没有可配置的思考档位";
    elements.thinkingVariantButton.setAttribute("aria-label", elements.thinkingVariantButton.title);
  }

  function renderThinkingVariantModels() {
    const models = state.thinkingVariantModels;
    if (!models.some((model) => modelKey(model) === state.thinkingVariantActiveKey)) {
      state.thinkingVariantActiveKey = models.length ? modelKey(models[0]) : null;
    }
    const fragment = document.createDocumentFragment();
    for (const model of models) {
      const key = modelKey(model);
      const selected = key === state.thinkingVariantActiveKey;
      const display = thinkingVariantDisplay(model);
      const button = document.createElement("button");
      button.type = "button";
      button.className = "thinking-model-option";
      button.dataset.modelKey = key;
      button.setAttribute("role", "option");
      button.setAttribute("aria-selected", String(selected));
      button.tabIndex = selected ? 0 : -1;
      button.title = `${display.provider} / ${model.model}`;
      const name = document.createElement("span");
      name.className = "thinking-model-option-name";
      name.textContent = display.name;
      const level = document.createElement("span");
      level.className = "thinking-model-option-level";
      level.textContent = thinkingVariantLabel(model.selected, true);
      button.append(name, level);
      button.addEventListener("click", () => {
        state.thinkingVariantActiveKey = key;
        renderThinkingVariantMenu();
        Array.from(elements.thinkingModelList.querySelectorAll(".thinking-model-option"))
          .find((option) => option.dataset.modelKey === key)?.focus();
      });
      fragment.appendChild(button);
    }
    if (!models.length) {
      const empty = document.createElement("div");
      empty.className = "thinking-variant-empty";
      empty.textContent = "当前模型没有可配置的思考档位";
      fragment.appendChild(empty);
    }
    elements.thinkingModelList.replaceChildren(fragment);
  }

  function renderThinkingVariantLevels() {
    const model = thinkingVariantModel();
    elements.thinkingLevelList.replaceChildren();
    if (!model) {
      elements.thinkingModelName.textContent = "";
      elements.thinkingModelProvider.textContent = "";
      return;
    }
    const display = thinkingVariantDisplay(model);
    elements.thinkingModelName.textContent = display.name;
    elements.thinkingModelName.title = model.model;
    elements.thinkingModelProvider.textContent = `${display.provider} / ${model.model}`;
    elements.thinkingModelProvider.title = elements.thinkingModelProvider.textContent;
    const fragment = document.createDocumentFragment();
    for (const variant of [null, ...model.variants]) {
      const selected = model.selected === variant;
      const button = document.createElement("button");
      button.type = "button";
      button.className = "thinking-level-option";
      button.setAttribute("role", "radio");
      button.setAttribute("aria-checked", String(selected));
      button.tabIndex = selected ? 0 : -1;
      button.title = variant == null ? "使用模型默认设置" : String(variant);
      const check = document.createElement("span");
      check.className = "thinking-level-check";
      check.setAttribute("aria-hidden", "true");
      const label = document.createElement("span");
      label.className = "thinking-level-name";
      label.textContent = thinkingVariantLabel(variant);
      button.append(check, label);
      button.addEventListener("click", () => selectThinkingVariant(modelKey(model), variant));
      fragment.appendChild(button);
    }
    elements.thinkingLevelList.appendChild(fragment);
  }

  function renderThinkingVariantMenu() {
    renderThinkingVariantModels();
    renderThinkingVariantLevels();
    // 单模型（非混合池）没有可选的模型，跳过模型栏，点开即是档位单选。
    const singleModel = state.thinkingVariantModels.length <= 1;
    const modelPane = elements.thinkingModelList.closest(".thinking-model-pane");
    if (modelPane) {
      modelPane.hidden = singleModel;
      modelPane.closest(".thinking-variant-layout")?.classList.toggle("single-model", singleModel);
    }
    updateThinkingVariantTrigger();
    positionThinkingVariantPopover();
  }

  function positionThinkingVariantPopover() {
    const popover = elements.thinkingVariantPopover;
    if (popover.hidden) return;
    const trigger = elements.thinkingVariantButton.getBoundingClientRect();
    const margin = 9;
    const gap = 8;
    const availableWidth = Math.max(180, window.innerWidth - margin * 2);
    popover.style.maxWidth = `${visualPixelsToLayout(availableWidth)}px`;
    popover.style.minWidth = `${Math.min(286, visualPixelsToLayout(availableWidth))}px`;
    popover.style.maxHeight = `${visualPixelsToLayout(Math.max(150, trigger.top - margin - gap))}px`;
    const measuredWidth = popover.offsetWidth * UI_SCALE;
    const measuredHeight = popover.offsetHeight * UI_SCALE;
    const left = Math.min(
      Math.max(margin, trigger.left),
      window.innerWidth - measuredWidth - margin
    );
    const top = Math.max(margin, trigger.top - measuredHeight - gap);
    popover.style.left = `${visualPixelsToLayout(left)}px`;
    popover.style.top = `${visualPixelsToLayout(top)}px`;
  }

  function openThinkingVariantPopover() {
    if (elements.thinkingVariantButton.disabled || !state.thinkingVariantModels.length) return;
    closeModelMenu();
    renderThinkingVariantMenu();
    elements.thinkingVariantPopover.hidden = false;
    elements.thinkingVariantButton.setAttribute("aria-expanded", "true");
    window.requestAnimationFrame(() => {
      positionThinkingVariantPopover();
      elements.thinkingLevelList.querySelector('[aria-checked="true"]')?.focus();
    });
  }

  function closeThinkingVariantPopover({ restoreFocus = false } = {}) {
    if (elements.thinkingVariantPopover.hidden) return;
    elements.thinkingVariantPopover.hidden = true;
    elements.thinkingVariantButton.setAttribute("aria-expanded", "false");
    if (restoreFocus) elements.thinkingVariantButton.focus();
  }

  async function loadThinkingVariants() {
    const generation = ++state.thinkingVariantLoadGeneration;
    state.thinkingVariantLoading = true;
    state.thinkingVariantError = "";
    updateControlState();
    try {
      const response = await apiRequest("/api/models/thinking-variants", { cache: "no-store" });
      const payload = await response.json();
      if (generation !== state.thinkingVariantLoadGeneration) return;
      state.thinkingVariantModels = normalizeThinkingVariantModels(payload?.options);
      state.thinkingVariantConfirmed = new Map(
        state.thinkingVariantModels.map((model) => [modelKey(model), model.selected])
      );
      state.thinkingVariantRevisions.clear();
      renderThinkingVariantMenu();
    } catch (error) {
      if (generation !== state.thinkingVariantLoadGeneration) return;
      state.thinkingVariantError = error.message || "无法载入思考档位";
      if (!state.thinkingVariantModels.length) closeThinkingVariantPopover();
      updateThinkingVariantTrigger();
    } finally {
      if (generation === state.thinkingVariantLoadGeneration) {
        state.thinkingVariantLoading = false;
        updateControlState();
      }
    }
  }

  function selectThinkingVariant(key, variant) {
    const model = thinkingVariantModel(key);
    if (!model || model.selected === variant) return;
    model.selected = variant;
    state.thinkingVariantRevisions.set(key, (state.thinkingVariantRevisions.get(key) || 0) + 1);
    renderThinkingVariantMenu();
    elements.thinkingLevelList.querySelector('[aria-checked="true"]')?.focus();
    state.thinkingVariantWriteChain = state.thinkingVariantWriteChain
      .catch(() => {})
      .then(() => persistLatestThinkingVariant(key));
  }

  async function persistLatestThinkingVariant(key) {
    const model = thinkingVariantModel(key);
    if (!model) return;
    const desired = model.selected;
    const confirmed = state.thinkingVariantConfirmed.has(key)
      ? state.thinkingVariantConfirmed.get(key)
      : null;
    if (desired === confirmed) return;
    const revision = state.thinkingVariantRevisions.get(key) || 0;
    try {
      const response = await apiRequest("/api/models/thinking-variants", {
        method: "PUT",
        body: JSON.stringify({
          updates: [{
            provider_id: model.provider_id,
            model: model.model,
            selected: desired
          }]
        })
      });
      const payload = await response.json();
      const returned = normalizeThinkingVariantModels(payload?.options)
        .find((candidate) => modelKey(candidate) === key);
      const applied = returned ? returned.selected : desired;
      state.thinkingVariantConfirmed.set(key, applied);
      const current = thinkingVariantModel(key);
      if (current && state.thinkingVariantRevisions.get(key) === revision && current.selected !== applied) {
        current.selected = applied;
        renderThinkingVariantMenu();
      } else {
        updateThinkingVariantTrigger();
      }
    } catch (error) {
      const current = thinkingVariantModel(key);
      if (current && state.thinkingVariantRevisions.get(key) === revision) {
        current.selected = confirmed;
        renderThinkingVariantMenu();
        showToast(error.message || "思考程度未保存", "error");
      }
    }
  }

  function handleThinkingVariantKeydown(event) {
    const inModels = event.target.closest("#thinkingModelList");
    const inLevels = event.target.closest("#thinkingLevelList");
    const container = inModels || inLevels;
    if (!container) return;
    const selector = inModels ? ".thinking-model-option" : ".thinking-level-option";
    const items = Array.from(container.querySelectorAll(selector));
    const index = items.indexOf(document.activeElement);
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      const direction = event.key === "ArrowDown" ? 1 : -1;
      items[(index + direction + items.length) % items.length]?.focus();
    } else if (event.key === "Home" || event.key === "End") {
      event.preventDefault();
      items[event.key === "Home" ? 0 : items.length - 1]?.focus();
    } else if (event.key === "ArrowRight" && inModels) {
      event.preventDefault();
      (elements.thinkingLevelList.querySelector('[aria-checked="true"]')
        || elements.thinkingLevelList.querySelector(".thinking-level-option"))?.focus();
    } else if (event.key === "ArrowLeft" && inLevels) {
      event.preventDefault();
      (elements.thinkingModelList.querySelector('[aria-selected="true"]')
        || elements.thinkingModelList.querySelector(".thinking-model-option"))?.focus();
    }
  }

  function closeSidebar() {
    elements.sidebar.classList.remove("open");
    elements.sidebarScrim.classList.remove("visible");
    elements.sidebarScrim.tabIndex = -1;
  }

  function setSidebarCollapsed(collapsed, { automatic = false } = {}) {
    state.sidebarCollapsed = Boolean(collapsed);
    state.sidebarAutoCollapsed = Boolean(automatic && collapsed);
    elements.appShell?.classList.toggle("is-sidebar-collapsed", state.sidebarCollapsed);
    if (elements.sidebarExpandButton) elements.sidebarExpandButton.hidden = !state.sidebarCollapsed;
    if (elements.sidebarCollapseButton) elements.sidebarCollapseButton.hidden = state.sidebarCollapsed;
    if (state.sidebarCollapsed) closeSidebar();
    if (!automatic) safeStorageSet("miyu.web.sidebarCollapsed", String(state.sidebarCollapsed));
    syncArtifactLayout?.();
  }

  function syncSidebarSpace() {
    if (layoutViewportWidth() <= 760) {
      if (state.sidebarAutoCollapsed) setSidebarCollapsed(false, { automatic: true });
      return;
    }
    const shellWidth = elements.appShell.clientWidth;
    const sidebarWidth = Number.parseFloat(getComputedStyle(elements.appShell).getPropertyValue("--sidebar-width")) || 252;
    const artifactWidth = state.artifactOpen && !state.artifactMaximized ? artifactWidthPixels() + 26 : 0;
    const availableWhenExpanded = shellWidth - sidebarWidth - artifactWidth;
    if (!state.sidebarCollapsed && availableWhenExpanded < 360) {
      setSidebarCollapsed(true, { automatic: true });
    } else if (state.sidebarAutoCollapsed && availableWhenExpanded >= 420) {
      setSidebarCollapsed(false, { automatic: true });
    }
  }

  function openSidebar(opener = document.activeElement) {
    state.sidebarOpener = opener;
    elements.sidebar.classList.add("open");
    elements.sidebarScrim.classList.add("visible");
    elements.sidebarScrim.tabIndex = 0;
  }

  function getFocusable(container) {
    return Array.from(container.querySelectorAll("button:not(:disabled), input:not(:disabled), textarea:not(:disabled), a[href], [tabindex]:not([tabindex='-1'])"))
      .filter((node) => !node.hidden && node.getClientRects().length > 0);
  }

  function openSettings(opener = document.activeElement) {
    state.settingsOpener = opener;
    closeModelMenu();
    closeThinkingVariantPopover();
    elements.settingsDrawer.classList.add("open");
    elements.settingsDrawer.setAttribute("aria-hidden", "false");
    elements.drawerScrim.classList.add("visible");
    elements.drawerScrim.tabIndex = 0;
    window.requestAnimationFrame(() => elements.settingsClose.focus());
    if (!state.configLoaded && !state.configLoading) loadConfigDraft();
  }

  function closeSettings({ restoreFocus = true } = {}) {
    if (!elements.settingsDrawer.classList.contains("open")) return;
    elements.settingsDrawer.classList.remove("open");
    elements.settingsDrawer.setAttribute("aria-hidden", "true");
    elements.drawerScrim.classList.remove("visible");
    elements.drawerScrim.tabIndex = -1;
    if (restoreFocus && state.settingsOpener instanceof HTMLElement) state.settingsOpener.focus();
    state.settingsOpener = null;
  }

  function openModelMenu() {
    if (elements.modelButton.disabled || state.models.length === 0) return;
    closeThinkingVariantPopover();
    resetModelMenuStaging();
    renderModelMenu();
    elements.modelMenu.hidden = false;
    elements.modelButton.setAttribute("aria-expanded", "true");
    refreshSessionModelOverride();
    const selected = elements.modelMenu.querySelector(".model-menu-item.selected:not(:disabled)");
    const first = elements.modelMenu.querySelector(".model-menu-item:not(:disabled)");
    window.requestAnimationFrame(() => (selected || first)?.focus());
  }

  function closeModelMenu({ restoreFocus = false, discard = true } = {}) {
    if (elements.modelMenu.hidden) return;
    elements.modelMenu.hidden = true;
    elements.modelButton.setAttribute("aria-expanded", "false");
    if (discard) {
      state.stagedModelKeys = null;
      state.stagedFollowGlobal = false;
      state.modelMenuTouched = false;
      state.modelMenuError = "";
    }
    if (restoreFocus) elements.modelButton.focus();
  }

  function showToast(message, type = "info") {
    const toast = document.createElement("div");
    toast.className = `toast${type === "error" ? " is-error" : ""}`;
    toast.textContent = String(message || "操作未完成");
    elements.toastRegion.replaceChildren(toast);
    if (state.toastTimer) window.clearTimeout(state.toastTimer);
    state.toastTimer = window.setTimeout(() => {
      if (toast.isConnected) toast.remove();
    }, type === "error" ? 6000 : 3000);
  }

  function showInlineError(message) {
    const text = String(message || "操作未完成").trim();
    elements.errorRegion.textContent = text;
    elements.errorRegion.hidden = !text;
  }

  function clearInlineError() {
    elements.errorRegion.textContent = "";
    elements.errorRegion.hidden = true;
  }

  function deepClone(value) {
    if (typeof structuredClone === "function") return structuredClone(value);
    return JSON.parse(JSON.stringify(value));
  }

  function normalizePersona(value) {
    const name = String(value?.name || "").trim() || "Miyu";
    const avatarUrl = typeof value?.avatar_url === "string" && value.avatar_url ? value.avatar_url : null;
    const boardImageUrl = typeof value?.board_image_url === "string" && value.board_image_url
      ? value.board_image_url
      : null;
    const boardTitle = String(value?.board_title || "").trim() || DEFAULT_BOARD_TITLE;
    const boardSubtitle = String(value?.board_subtitle || "").trim() || DEFAULT_BOARD_SUBTITLE;
    const configuredPrompts = Array.isArray(value?.starter_prompts) ? value.starter_prompts : [];
    const starterPrompts = DEFAULT_STARTER_PROMPTS.map((fallback, index) => String(configuredPrompts[index] || "").trim() || fallback);
    // revision 只在图片 URL 真正变化时更新:每次快照都取 Date.now() 会让
    // 头像/看板图的浏览器缓存永远击穿,每次 bootstrap 都重新下载。
    const previous = state.persona;
    const revision =
      previous && previous.avatar_url === avatarUrl && previous.board_image_url === boardImageUrl
        ? previous.revision
        : `${Date.now()}`;
    return {
      name,
      avatar_url: avatarUrl,
      board_image_url: boardImageUrl,
      board_title: boardTitle,
      board_subtitle: boardSubtitle,
      starter_prompts: starterPrompts,
      revision
    };
  }

  function setPersonaAvatar(image) {
    const url = state.persona?.avatar_url;
    image.hidden = !url;
    if (!url) {
      image.removeAttribute("src");
      return;
    }
    image.hidden = false;
    const separator = url.includes("?") ? "&" : "?";
    image.src = `${url}${separator}v=${encodeURIComponent(state.persona?.revision || "1")}`;
    image.onerror = () => {
      image.hidden = true;
      image.removeAttribute("src");
    };
  }

  function applyPersona(value) {
    state.persona = normalizePersona(value);
    elements.brandName.textContent = state.persona.name;
    elements.brandAvatar.alt = state.persona.name;
    setPersonaAvatar(elements.brandAvatar);
    elements.emptyKickerName.textContent = state.persona.name;
    elements.emptyTitle.textContent = state.persona.board_title;
    elements.emptySubtitle.textContent = state.persona.board_subtitle;
    const boardImageUrl = state.persona.board_image_url;
    elements.emptyVisual.hidden = !boardImageUrl;
    elements.emptyBoardImage.alt = `${state.persona.name} 看板图片`;
    if (boardImageUrl) {
      elements.emptyBoardImage.onerror = () => {
        elements.emptyBoardImage.removeAttribute("src");
        elements.emptyVisual.hidden = true;
      };
      elements.emptyBoardImage.src = `${boardImageUrl}${boardImageUrl.includes("?") ? "&" : "?"}v=${encodeURIComponent(state.persona.revision)}`;
    } else {
      elements.emptyBoardImage.removeAttribute("src");
    }
    elements.promptGrid.querySelectorAll("[data-prompt]").forEach((button, index) => {
      const prompt = state.persona.starter_prompts[index] || DEFAULT_STARTER_PROMPTS[index];
      button.dataset.prompt = prompt;
      const label = button.querySelector("span:last-child");
      if (label) label.textContent = prompt;
    });
    const refreshAssistant = (root) => root.querySelectorAll(".assistant-label").forEach((label) => {
      const name = label.querySelector("strong");
      const avatar = label.querySelector("img");
      if (name) name.textContent = state.persona.name;
      if (avatar) setPersonaAvatar(avatar);
    });
    refreshAssistant(elements.timeline);
    for (const articles of state.finishedTurnArticles.values()) {
      for (const entry of articles) refreshAssistant(entry.article);
    }
  }

  function setSettingsView(view) {
    const selected = ["interface", "prompts", "general", "providers", "models", "plugins", "advanced"].includes(view) ? view : "interface";
    state.settingsView = selected;
    elements.settingsNav.querySelectorAll("[data-settings-view]").forEach((button) => {
      const active = button.dataset.settingsView === selected;
      button.classList.toggle("active", active);
      button.setAttribute("aria-current", active ? "page" : "false");
    });
    elements.settingsPanels.forEach((panel) => {
      panel.hidden = panel.dataset.settingsPanel !== selected;
    });
  }

  function configValue(path, fallback = undefined) {
    let value = state.configDraft;
    for (const key of path.split(".")) {
      if (value == null || typeof value !== "object" || !(key in value)) return fallback;
      value = value[key];
    }
    return value;
  }

  function setConfigValue(path, value) {
    if (!state.configDraft) return;
    const keys = path.split(".");
    let target = state.configDraft;
    for (const key of keys.slice(0, -1)) {
      if (!target[key] || typeof target[key] !== "object") target[key] = {};
      target = target[key];
    }
    target[keys[keys.length - 1]] = value;
    markConfigDirty();
  }

  function clearConfigFieldError(input) {
    const message = state.invalidConfigFields.get(input);
    if (message) message.remove();
    state.invalidConfigFields.delete(input);
    input.classList.remove("is-invalid");
  }

  function setConfigFieldError(input, message) {
    clearConfigFieldError(input);
    const error = document.createElement("small");
    error.className = "config-field-error";
    error.textContent = message;
    input.classList.add("is-invalid");
    input.closest(".config-field")?.appendChild(error);
    state.invalidConfigFields.set(input, error);
  }

  function parseConfigInput(input, current) {
    clearConfigFieldError(input);
    if (input.dataset.valueType === "boolean") return input.checked;
    const raw = input.value;
    if (input.dataset.valueType === "number") {
      const number = Number(raw);
      if (!Number.isFinite(number)) throw new Error("请输入有效数字");
      return input.dataset.integer === "true" ? Math.trunc(number) : number;
    }
    if (input.dataset.valueType === "json") {
      if (!raw.trim()) return input.dataset.nullable === "true" ? null : {};
      try {
        return JSON.parse(raw);
      } catch (_) {
        throw new Error("请输入有效 JSON");
      }
    }
    if (input.dataset.valueType === "lines") {
      return raw.split(/\r?\n|,/).map((item) => item.trim()).filter(Boolean);
    }
    if (input.dataset.valueType === "numbers") {
      return raw.split(/[\s,;，；]+/).filter(Boolean).map((item) => {
        const number = Number(item);
        if (!Number.isSafeInteger(number)) throw new Error(`无效号码：${item}`);
        return number;
      });
    }
    return raw;
  }

  function bindConfigInput(input, path, options = {}) {
    input.dataset.configPath = path;
    input.dataset.valueType = options.type || "string";
    if (options.integer) input.dataset.integer = "true";
    if (options.nullable) input.dataset.nullable = "true";
    const eventName = input.tagName === "SELECT" || input.type === "checkbox" ? "change" : "input";
    input.addEventListener(eventName, () => {
      try {
        const value = parseConfigInput(input, configValue(path));
        setConfigValue(path, value);
        updateAdvancedConfigEditor();
        if (options.rerender) renderConfigEditors();
      } catch (error) {
        setConfigFieldError(input, error.message);
        updateSettingsControls();
      }
    });
    return input;
  }

  function configField(labelText, input, description = "") {
    const label = document.createElement("label");
    label.className = "config-field";
    const heading = document.createElement("span");
    heading.className = "config-field-label";
    heading.textContent = labelText;
    label.append(heading, input);
    if (description) {
      const hint = document.createElement("small");
      hint.className = "config-field-hint";
      hint.textContent = description;
      label.appendChild(hint);
    }
    return label;
  }

  function textConfigField(label, path, options = {}) {
    const current = configValue(path, options.defaultValue ?? "");
    const input = options.multiline ? document.createElement("textarea") : document.createElement("input");
    input.className = "config-input";
    if (!options.multiline) input.type = options.inputType || "text";
    if (options.multiline) input.rows = options.rows || 3;
    input.value = options.type === "json"
      ? (current == null ? "" : JSON.stringify(current, null, 2))
      : options.type === "lines"
        ? (Array.isArray(current) ? current.join("\n") : "")
        : options.type === "numbers"
          ? (Array.isArray(current) ? current.join(", ") : "")
          : String(current ?? "");
    if (options.placeholder) input.placeholder = options.placeholder;
    if (options.min != null) input.min = String(options.min);
    if (options.max != null) input.max = String(options.max);
    if (options.step != null) input.step = String(options.step);
    bindConfigInput(input, path, options);
    return configField(label, input, options.description || "");
  }

  function selectConfigField(label, path, choices, description = "") {
    const select = document.createElement("select");
    select.className = "config-input";
    const current = String(configValue(path, ""));
    for (const choice of choices) {
      const option = document.createElement("option");
      option.value = typeof choice === "string" ? choice : choice.value;
      option.textContent = typeof choice === "string" ? choice : choice.label;
      option.selected = option.value === current;
      select.appendChild(option);
    }
    bindConfigInput(select, path);
    return configField(label, select, description);
  }

  function booleanConfigField(labelText, path, description = "") {
    const label = document.createElement("label");
    label.className = "config-toggle";
    const input = document.createElement("input");
    input.type = "checkbox";
    input.checked = Boolean(configValue(path));
    bindConfigInput(input, path, { type: "boolean" });
    const switchTrack = document.createElement("span");
    switchTrack.className = "toggle-track";
    const copy = document.createElement("span");
    copy.className = "config-toggle-copy";
    const title = document.createElement("strong");
    title.textContent = labelText;
    copy.appendChild(title);
    if (description) {
      const hint = document.createElement("small");
      hint.textContent = description;
      copy.appendChild(hint);
    }
    label.append(input, switchTrack, copy);
    return label;
  }

  function configGroup(titleText, fields = [], description = "") {
    const group = document.createElement("section");
    group.className = "config-group";
    const header = document.createElement("header");
    const title = document.createElement("h3");
    title.textContent = titleText;
    header.appendChild(title);
    if (description) {
      const copy = document.createElement("p");
      copy.textContent = description;
      header.appendChild(copy);
    }
    const body = document.createElement("div");
    body.className = "config-group-body";
    body.append(...fields);
    group.append(header, body);
    return group;
  }

  function actionButton(label, className = "secondary-button") {
    const button = document.createElement("button");
    button.type = "button";
    button.className = className;
    button.textContent = label;
    return button;
  }

  function markConfigDirty() {
    state.configDirty = true;
    updateSettingsControls();
  }

  function clearProviderSecretChanges() {
    for (const key of Object.keys(state.secretChanges)) {
      if (key.startsWith("providers.")) delete state.secretChanges[key];
    }
  }

  function refreshProviderSecretStates() {
    for (const key of Object.keys(state.secretStates)) {
      if (key.startsWith("providers.")) delete state.secretStates[key];
    }
    state.providerSecretStates.forEach((configured, index) => {
      state.secretStates[`providers.${index}.api_key`] = Boolean(configured);
    });
  }

  function updateSettingsControls() {
    const busy = state.configLoading || state.configSaving;
    elements.reloadConfigButton.disabled = busy;
    elements.saveConfigButton.disabled = busy || !state.configLoaded || !state.configDirty || state.invalidConfigFields.size > 0 || conversationRunning();
    elements.addProviderButton.disabled = busy || !state.configLoaded;
    if (state.configLoading) elements.settingsStatus.textContent = "正在载入配置";
    else if (state.configSaving) elements.settingsStatus.textContent = "正在验证并保存";
    else if (!state.configLoaded) elements.settingsStatus.textContent = "尚未载入配置";
    else if (state.invalidConfigFields.size) elements.settingsStatus.textContent = "请修正表单中的错误";
    else if (conversationRunning() && state.configDirty) elements.settingsStatus.textContent = "回复完成后才能保存";
    else elements.settingsStatus.textContent = state.configDirty ? "有未保存的修改" : "配置已同步";
  }

  function updateAdvancedConfigEditor() {
    if (!state.configDraft || document.activeElement === elements.advancedConfigEditor) return;
    elements.advancedConfigEditor.value = JSON.stringify(state.configDraft, null, 2);
  }

  function renderGeneralConfig() {
    elements.generalConfigForm.replaceChildren(
      configGroup("工具", [
        booleanConfigField("启用工具", "tools.enabled"),
        textConfigField("最大工具轮数", "tools.max_rounds", { type: "number", integer: true, inputType: "number", min: 0 }),
        selectConfigField("工具加载模式", "tools.loading_mode", ["full", "hybrid"]),
        booleanConfigField("记住已加载工具", "tools.persist_loaded_tools")
      ]),
      configGroup("Skills", [
        booleanConfigField("启用 Skills", "skills.enabled"),
        booleanConfigField("允许执行命令", "skills.allow_command_execution")
      ]),
      configGroup("思考", [
        selectConfigField(
          "思考详细程度",
          "display.reasoning",
          [{ value: "summary", label: "摘要" }, { value: "full", label: "完整" }, { value: "hidden", label: "隐藏" }],
          "决定向模型请求摘要还是完整思考并写入会话；设为隐藏则不产生思考内容。WebUI 的展开/收起在「界面」里设置。"
        )
      ]),
      configGroup("上下文", [
        selectConfigField("到达上限后", "context.on_overflow", [{ value: "compact", label: "压缩上下文" }, { value: "pop", label: "弹出旧消息" }]),
        textConfigField("开始裁剪比例", "context.trim_at_ratio", { type: "number", inputType: "number", min: 0.1, max: 1, step: 0.01 }),
        textConfigField("每批裁剪比例", "context.trim_batch_ratio", { type: "number", inputType: "number", min: 0.01, max: 0.9, step: 0.01 })
      ]),
      configGroup("记忆", [
        booleanConfigField("启用记忆", "memory.enabled"),
        booleanConfigField("保留弹出上下文", "memory.evicted_context_enabled"),
        booleanConfigField("启用联想", "memory.association_enabled"),
        booleanConfigField("自动日记", "memory.auto_diary_enabled"),
        booleanConfigField("自动事实记忆", "memory.auto_fact_enabled"),
        textConfigField("日记整理轮数", "memory.diary_batch_size", { type: "number", inputType: "number", integer: true, min: 2, max: 100 }),
        textConfigField("短期日记保留天数", "memory.short_diary_retention_days", { type: "number", inputType: "number", integer: true, min: 1, max: 3650 }),
        textConfigField("日记长期化召回次数", "memory.diary_promotion_recalls", { type: "number", inputType: "number", integer: true, min: 1, max: 100 }),
        textConfigField("记忆整理超时秒数", "memory.organizer_timeout_seconds", { type: "number", inputType: "number", integer: true, min: 5, max: 600 }),
        textConfigField("联想知识条数", "memory.association_facts", { type: "number", inputType: "number", integer: true, min: 0 }),
        textConfigField("联想事件条数", "memory.association_episodes", { type: "number", inputType: "number", integer: true, min: 0 }),
        textConfigField("联想字符上限", "memory.association_max_chars", { type: "number", inputType: "number", integer: true, min: 0 }),
        textConfigField("片段字符数", "memory.snippet_chars", { type: "number", inputType: "number", integer: true, min: 0 }),
        textConfigField("遗忘期限（天）", "memory.forget_after_days", { type: "number", inputType: "number", integer: true, min: 1 }),
        booleanConfigField("启用遗忘", "memory.forgetting_enabled"),
        textConfigField("遗忘半衰期（天）", "memory.forgetting_half_life_days", { type: "number", inputType: "number", min: 0.1, step: 0.1 }),
        textConfigField("最低遗忘强度", "memory.forgetting_min_strength", { type: "number", inputType: "number", min: 0, max: 1, step: 0.01 }),
        textConfigField("回忆增强强度", "memory.forgetting_review_boost", { type: "number", inputType: "number", min: 0, step: 0.01 }),
        textConfigField("最小任务字数", "memory.learning_min_task_chars", { type: "number", inputType: "number", integer: true, min: 0 }),
        textConfigField("最小方法字数", "memory.learning_min_method_chars", { type: "number", inputType: "number", integer: true, min: 0 })
      ]),
      configGroup("MCP", [
        booleanConfigField("启用 MCP", "mcp.enabled"),
        textConfigField("服务器配置", "mcp.servers", { type: "json", multiline: true, rows: 10, description: "JSON 数组，支持 id、command、args、env、timeout_seconds 和 enabled。" })
      ])
    );
  }

  function secretEditor(labelText, key, { multiline = false } = {}) {
    const wrapper = document.createElement("div");
    wrapper.className = "secret-editor config-field";
    const label = document.createElement("span");
    label.className = "config-field-label";
    label.textContent = labelText;
    const status = document.createElement("small");
    status.className = "secret-status";
    status.textContent = state.secretChanges[key]?.action === "clear"
      ? "将清空"
      : state.secretChanges[key]?.action === "set"
        ? "已输入新值"
        : state.secretStates[key]
          ? "已配置"
          : "未配置";
    const input = multiline ? document.createElement("textarea") : document.createElement("input");
    input.className = "config-input";
    if (!multiline) input.type = "password";
    if (multiline) input.rows = 3;
    input.placeholder = state.secretStates[key] ? "留空保留现有值" : "输入新值";
    input.value = state.secretChanges[key]?.action === "set" ? state.secretChanges[key].value : "";
    input.autocomplete = "new-password";
    const actions = document.createElement("div");
    actions.className = "secret-actions";
    const clear = actionButton("清空", "text-button danger-text");
    const preserve = actionButton("保留", "text-button");
    actions.append(preserve, clear);
    input.addEventListener("input", () => {
      if (input.value) state.secretChanges[key] = { action: "set", value: input.value };
      else delete state.secretChanges[key];
      markConfigDirty();
      status.textContent = input.value ? "已输入新值" : state.secretStates[key] ? "已配置" : "未配置";
    });
    clear.addEventListener("click", () => {
      input.value = "";
      state.secretChanges[key] = { action: "clear" };
      status.textContent = "将清空";
      markConfigDirty();
    });
    preserve.addEventListener("click", () => {
      input.value = "";
      delete state.secretChanges[key];
      status.textContent = state.secretStates[key] ? "已配置" : "未配置";
      markConfigDirty();
    });
    wrapper.append(label, status, input, actions);
    return wrapper;
  }

  function ensureProviderDefaults(provider = {}) {
    return {
      id: "",
      display_name: "",
      base_url: "",
      protocol: "auto",
      api_key: null,
      models: [],
      model_context_window: {},
      model_costs: {},
      model_modalities: {},
      default_model: "",
      timeout_seconds: 60,
      temperature: 1.0,
      anthropic_max_tokens: 4096,
      extra_body: null,
      ...provider
    };
  }

  const PLATFORM_MODEL_POOL_NAMES = ["text_models", "multimodal_models", "non_whitelist_text_models"];

  function forEachPlatformModelPool(callback) {
    const qq = state.configDraft?.platforms?.qq;
    if (!qq || typeof qq !== "object") return;
    for (const poolName of PLATFORM_MODEL_POOL_NAMES) {
      if (Array.isArray(qq[poolName])) callback(qq, poolName, qq[poolName]);
    }
    const realContext = qq.plugins?.real_context?.settings;
    if (Array.isArray(realContext?.text_models)) {
      callback(realContext, "text_models", realContext.text_models);
    }
    for (const route of Array.isArray(qq.conversations) ? qq.conversations : []) {
      if (!route || typeof route !== "object") continue;
      for (const poolName of PLATFORM_MODEL_POOL_NAMES) {
        if (Array.isArray(route[poolName])) callback(route, poolName, route[poolName]);
      }
    }
  }

  function normalizePlatformModelRoutes() {
    forEachPlatformModelPool((owner, poolName, pool) => {
      if (pool.length === 0) delete owner[poolName];
    });
  }

  function replacePlatformProviderReferences(previousId, nextId) {
    forEachPlatformModelPool((_route, _poolName, pool) => {
      for (const item of pool) {
        if (item?.provider_id === previousId) item.provider_id = nextId;
      }
    });
  }

  function removePlatformProviderReferences(providerId) {
    forEachPlatformModelPool((route, poolName, pool) => {
      route[poolName] = pool.filter((item) => item?.provider_id !== providerId);
    });
    normalizePlatformModelRoutes();
  }

  function providerHasConfiguredModel(provider, model) {
    const normalizedModel = String(model || "").trim();
    return Boolean(normalizedModel) && (
      String(provider?.default_model || "") === normalizedModel
      || (Array.isArray(provider?.models) && provider.models.includes(normalizedModel))
    );
  }

  function forEachSubagentTierPool(callback) {
    const tiers = state.configDraft?.subagent_tiers;
    if (!tiers || typeof tiers !== "object") return;
    for (const [tierName, pool] of Object.entries(tiers)) {
      if (Array.isArray(pool)) callback(tiers, tierName, pool);
    }
  }

  function pruneOptionalPool(owner, key, predicate) {
    if (!owner || !Array.isArray(owner[key])) return;
    const pool = owner[key].filter(predicate);
    if (pool.length) owner[key] = pool;
    else delete owner[key];
  }

  function providerModelSupportsMedia(provider, model) {
    const normalizedModel = String(model || "").trim();
    const declared = provider?.model_modalities;
    if (declared && typeof declared === "object" && Object.prototype.hasOwnProperty.call(declared, normalizedModel)) {
      return Array.isArray(declared[normalizedModel])
        && declared[normalizedModel].includes("image");
    }
    return state.configInferredImageModels.some((item) => (
      item?.provider_id === provider?.id && item?.model === normalizedModel
    ));
  }

  function modelReferenceTarget(providersById, item) {
    const provider = providersById.get(String(item?.provider_id || "").trim());
    const model = String(item?.model || "").trim();
    return provider && providerHasConfiguredModel(provider, model) ? { provider, model } : null;
  }

  function prunePlatformModelRoutes(providersById) {
    forEachPlatformModelPool((route, poolName, pool) => {
      route[poolName] = pool.filter((item) => {
        const target = modelReferenceTarget(providersById, item);
        return Boolean(target) && (
          poolName !== "multimodal_models"
          || providerModelSupportsMedia(target.provider, target.model)
        );
      });
    });
    normalizePlatformModelRoutes();
  }

  function clearInvalidPluginModelReferences(providersById) {
    const vision = state.configDraft?.plugins?.vision;
    if (vision?.vision_provider_id) {
      const provider = providersById.get(String(vision.vision_provider_id).trim());
      const configuredModel = String(vision.vision_model || "").trim();
      const model = configuredModel || String(provider?.default_model || "").trim();
      if (!provider || !providerHasConfiguredModel(provider, model) || !providerModelSupportsMedia(provider, model)) {
        vision.vision_provider_id = "";
        vision.vision_model = "";
      }
    }
    const knowledgeBase = state.configDraft?.plugins?.knowledge_base;
    if (knowledgeBase?.embedding_provider_id) {
      const provider = providersById.get(String(knowledgeBase.embedding_provider_id).trim());
      const configuredModel = String(knowledgeBase.embedding_model || "").trim();
      const model = configuredModel || String(provider?.default_model || "").trim();
      if (!provider || !providerHasConfiguredModel(provider, model)) {
        knowledgeBase.embedding_provider_id = "";
        knowledgeBase.embedding_model = "";
      }
    }
  }

  function pruneModelReferences() {
    if (!state.configDraft) return;
    const providers = Array.isArray(state.configDraft.providers) ? state.configDraft.providers : [];
    const providersById = new Map(providers.map((provider) => [String(provider?.id || ""), provider]));
    pruneOptionalPool(state.configDraft, "active_provider_models", (item) => (
      Boolean(modelReferenceTarget(providersById, item))
    ));
    pruneOptionalPool(state.configDraft, "active_multimodal_provider_models", (item) => {
      const target = modelReferenceTarget(providersById, item);
      return Boolean(target) && providerModelSupportsMedia(target.provider, target.model);
    });
    forEachSubagentTierPool((tiers, tierName, pool) => {
      tiers[tierName] = pool.filter((item) => Boolean(modelReferenceTarget(providersById, item)));
    });
    prunePlatformModelRoutes(providersById);
    clearInvalidPluginModelReferences(providersById);
  }

  function replaceProviderReferences(previousId, nextId) {
    if (!previousId || previousId === nextId || !state.configDraft) return;
    if (state.configDraft.active_provider === previousId) state.configDraft.active_provider = nextId;
    for (const poolName of ["active_provider_models", "active_multimodal_provider_models"]) {
      for (const item of state.configDraft[poolName] || []) {
        if (item.provider_id === previousId) item.provider_id = nextId;
      }
    }
    if (state.configDraft.plugins?.vision?.vision_provider_id === previousId) {
      state.configDraft.plugins.vision.vision_provider_id = nextId;
    }
    if (state.configDraft.plugins?.knowledge_base?.embedding_provider_id === previousId) {
      state.configDraft.plugins.knowledge_base.embedding_provider_id = nextId;
    }
    forEachSubagentTierPool((_tiers, _tierName, pool) => {
      for (const item of pool) {
        if (item?.provider_id === previousId) item.provider_id = nextId;
      }
    });
    replacePlatformProviderReferences(previousId, nextId);
    for (const models of [state.configMultimodalModels, state.configInferredImageModels]) {
      for (const model of models) {
        if (model?.provider_id === previousId) model.provider_id = nextId;
      }
    }
  }

  function removeProviderReferences(providerId) {
    if (!state.configDraft) return;
    pruneOptionalPool(state.configDraft, "active_provider_models", (item) => item?.provider_id !== providerId);
    pruneOptionalPool(state.configDraft, "active_multimodal_provider_models", (item) => item?.provider_id !== providerId);
    forEachSubagentTierPool((tiers, tierName, pool) => {
      tiers[tierName] = pool.filter((item) => item?.provider_id !== providerId);
    });
    if (state.configDraft.plugins?.vision?.vision_provider_id === providerId) {
      state.configDraft.plugins.vision.vision_provider_id = "";
      state.configDraft.plugins.vision.vision_model = "";
    }
    if (state.configDraft.plugins?.knowledge_base?.embedding_provider_id === providerId) {
      state.configDraft.plugins.knowledge_base.embedding_provider_id = "";
      state.configDraft.plugins.knowledge_base.embedding_model = "";
    }
    removePlatformProviderReferences(providerId);
    state.configMultimodalModels = state.configMultimodalModels.filter((item) => item?.provider_id !== providerId);
    state.configInferredImageModels = state.configInferredImageModels.filter((item) => item?.provider_id !== providerId);
  }

  function renderProviders() {
    elements.providerEditor.replaceChildren();
    const providers = Array.isArray(state.configDraft?.providers) ? state.configDraft.providers : [];
    providers.forEach((provider, index) => {
      let referencedProviderId = String(provider.id || "");
      const card = document.createElement("details");
      card.className = "provider-card";
      card.open = index === 0;
      const summary = document.createElement("summary");
      const copy = document.createElement("span");
      const name = document.createElement("strong");
      name.textContent = provider.display_name || provider.id || `供应商 ${index + 1}`;
      const id = document.createElement("small");
      id.textContent = provider.id || "尚未命名";
      copy.append(name, id);
      const remove = actionButton("", "icon-button danger-text");
      remove.title = "删除";
      remove.setAttribute("aria-label", "删除");
      remove.appendChild(makeIconSlot("trash-2"));
      remove.addEventListener("click", (event) => {
        event.preventDefault();
        event.stopPropagation();
        if (!window.confirm(`删除供应商“${provider.display_name || provider.id || index + 1}”？`)) return;
        state.configDraft.providers.splice(index, 1);
        state.providerSecretStates.splice(index, 1);
        refreshProviderSecretStates();
        clearProviderSecretChanges();
        const removedProviderId = referencedProviderId || provider.id;
        removeProviderReferences(removedProviderId);
        if (state.configDraft.active_provider === removedProviderId || state.configDraft.active_provider === provider.id) {
          state.configDraft.active_provider = state.configDraft.providers[0]?.id || "";
        }
        markConfigDirty();
        renderConfigEditors();
      });
      summary.append(copy, remove);
      const body = document.createElement("div");
      body.className = "provider-card-body";
      const fields = [
        ["配置 ID", "id"], ["显示名称", "display_name"], ["Base URL", "base_url"],
        ["默认模型", "default_model"]
      ];
      for (const [label, key] of fields) {
        const input = document.createElement("input");
        input.className = "config-input";
        input.value = String(provider[key] || "");
        input.addEventListener("input", () => {
          const previousId = key === "id" ? String(provider.id || "") : "";
          provider[key] = input.value;
          if (key === "id" && previousId !== provider.id) {
            const nextId = String(provider.id || "");
            if (referencedProviderId && nextId && referencedProviderId !== nextId) {
              replaceProviderReferences(referencedProviderId, nextId);
            }
            if (nextId) referencedProviderId = nextId;
            state.providerSecretStates[index] = false;
            delete state.secretChanges[`providers.${index}.api_key`];
            refreshProviderSecretStates();
            renderModelPools();
          }
          if (key === "default_model") renderModelPools();
          if (key === "display_name" || key === "id") {
            name.textContent = provider.display_name || provider.id || `供应商 ${index + 1}`;
            id.textContent = provider.id || "尚未命名";
          }
          markConfigDirty();
          updateAdvancedConfigEditor();
        });
        if (key === "default_model") {
          input.addEventListener("change", () => {
            provider.models = Array.isArray(provider.models) ? provider.models : [];
            if (provider.default_model && !provider.models.includes(provider.default_model)) {
              provider.models.push(provider.default_model);
            }
            pruneModelReferences();
            renderModelPools();
            updateAdvancedConfigEditor();
          });
        }
        body.appendChild(configField(label, input));
      }
      const protocol = document.createElement("select");
      protocol.className = "config-input";
      for (const value of ["auto", "openai-chat", "openai-responses", "anthropic"]) {
        const option = document.createElement("option");
        option.value = value;
        option.textContent = value;
        option.selected = provider.protocol === value;
        protocol.appendChild(option);
      }
      protocol.addEventListener("change", () => { provider.protocol = protocol.value; markConfigDirty(); updateAdvancedConfigEditor(); });
      body.appendChild(configField("协议", protocol));
      const secretKey = `providers.${index}.api_key`;
      body.appendChild(secretEditor("API Key", secretKey));

      const numeric = [
        ["超时秒数", "timeout_seconds", 1, 1], ["Temperature", "temperature", 0, 0.1], ["Anthropic 最大 Token", "anthropic_max_tokens", 1, 1]
      ];
      for (const [label, key, min, step] of numeric) {
        const input = document.createElement("input");
        input.className = "config-input";
        input.type = "number";
        input.min = String(min);
        input.step = String(step);
        input.value = String(provider[key] ?? "");
        input.addEventListener("input", () => {
          const value = Number(input.value);
          if (Number.isFinite(value)) {
            provider[key] = key === "temperature" ? value : Math.trunc(value);
            markConfigDirty();
            updateAdvancedConfigEditor();
          }
        });
        body.appendChild(configField(label, input));
      }
      const structured = [
        ["可用模型", "models", "lines", "每行一个模型"],
        ["模型上下文窗口", "model_context_window", "json", "JSON 对象：模型名到 Token 数"], ["模型价格", "model_costs", "json", "JSON 对象：模型名到 {currency, input, output, cache_read}，currency 可为 USD/CNY(默认 USD)，价格单位为 每 1M tokens；留空用 models.dev 目录价"],
        ["模型输入模态", "model_modalities", "json", "JSON 对象：模型名到 text/image/audio/video/pdf 数组"],
        ["额外请求体", "extra_body", "json", "JSON 对象，留空表示不设置"]
      ];
      for (const [label, key, type, description] of structured) {
        const input = document.createElement("textarea");
        input.className = "config-input";
        input.rows = key === "models" ? 4 : 5;
        input.value = type === "lines" ? (provider[key] || []).join("\n") : provider[key] == null ? "" : JSON.stringify(provider[key], null, 2);
        input.addEventListener("input", () => {
          clearConfigFieldError(input);
          try {
            provider[key] = type === "lines"
              ? input.value.split(/\r?\n|,/).map((item) => item.trim()).filter(Boolean)
              : input.value.trim() ? JSON.parse(input.value) : key === "extra_body" ? null : {};
            if (key === "models" && provider.default_model && !provider.models.includes(provider.default_model)) {
              provider.models.push(provider.default_model);
            }
            markConfigDirty();
            updateAdvancedConfigEditor();
            if (key === "models" || key === "model_modalities") renderModelPools();
          } catch (_) {
            setConfigFieldError(input, "请输入有效 JSON");
            updateSettingsControls();
          }
        });
        if (key === "models" || key === "model_modalities") {
          input.addEventListener("change", () => {
            if (state.invalidConfigFields.has(input)) return;
            pruneModelReferences();
            renderModelPools();
            updateAdvancedConfigEditor();
          });
        }
        body.appendChild(configField(label, input, description));
      }
      card.append(summary, body);
      elements.providerEditor.appendChild(card);
    });
    if (!providers.length) {
      const empty = document.createElement("p");
      empty.className = "settings-empty";
      empty.textContent = "至少需要添加一个供应商。";
      elements.providerEditor.appendChild(empty);
    }
  }

  function configuredModelChoices() {
    const result = [];
    for (const provider of state.configDraft?.providers || []) {
      const models = Array.isArray(provider.models) && provider.models.length ? provider.models : provider.default_model ? [provider.default_model] : [];
      for (const model of models) {
        if (String(model).trim()) result.push({ provider_id: String(provider.id || ""), provider_name: String(provider.display_name || provider.id || ""), model: String(model) });
      }
    }
    return result;
  }

  function renderModelPoolList(titleText, path, choices) {
    const providers = Array.isArray(state.configDraft?.providers) ? state.configDraft.providers : [];
    const selected = Array.isArray(state.configDraft[path])
      ? state.configDraft[path]
      : path === "active_provider_models"
        ? choices.filter((choice) => choice.provider_id === state.configDraft.active_provider && choice.model === providers.find((provider) => provider.id === state.configDraft.active_provider)?.default_model)
        : [];
    const group = configGroup(titleText);
    const body = group.querySelector(".config-group-body");
    if (!choices.length) {
      const empty = document.createElement("p");
      empty.className = "settings-empty";
      empty.textContent = "请先在供应商中配置模型。";
      body.appendChild(empty);
    }
    for (const model of choices) {
      const label = document.createElement("label");
      label.className = "model-pool-option";
      const input = document.createElement("input");
      input.type = "checkbox";
      input.checked = selected.some((item) => item.provider_id === model.provider_id && item.model === model.model);
      input.addEventListener("change", () => {
        let pool = Array.isArray(state.configDraft[path]) ? state.configDraft[path] : [...selected];
        if (input.checked && !pool.some((item) => item.provider_id === model.provider_id && item.model === model.model)) {
          pool = [...pool, { provider_id: model.provider_id, model: model.model }];
        } else if (!input.checked) {
          pool = pool.filter((item) => item.provider_id !== model.provider_id || item.model !== model.model);
        }
        state.configDraft[path] = pool;
        markConfigDirty();
        updateAdvancedConfigEditor();
      });
      const copy = document.createElement("span");
      const name = document.createElement("strong");
      name.textContent = model.model;
      const provider = document.createElement("small");
      provider.textContent = model.provider_name;
      copy.append(name, provider);
      label.append(input, copy);
      body.appendChild(label);
    }
    return group;
  }

  function renderSubagentTierList(titleText, tierKey, choices) {
    if (!state.configDraft.subagent_tiers || typeof state.configDraft.subagent_tiers !== "object") {
      state.configDraft.subagent_tiers = {};
    }
    const tiers = state.configDraft.subagent_tiers;
    const selected = Array.isArray(tiers[tierKey]) ? tiers[tierKey] : [];
    const group = configGroup(titleText);
    const body = group.querySelector(".config-group-body");
    if (!choices.length) {
      const empty = document.createElement("p");
      empty.className = "settings-empty";
      empty.textContent = "请先在供应商中配置模型。";
      body.appendChild(empty);
    }
    for (const model of choices) {
      const label = document.createElement("label");
      label.className = "model-pool-option";
      const input = document.createElement("input");
      input.type = "checkbox";
      input.checked = selected.some((item) => item.provider_id === model.provider_id && item.model === model.model);
      input.addEventListener("change", () => {
        let pool = Array.isArray(tiers[tierKey]) ? tiers[tierKey] : [];
        if (input.checked && !pool.some((item) => item.provider_id === model.provider_id && item.model === model.model)) {
          pool = [...pool, { provider_id: model.provider_id, model: model.model }];
        } else if (!input.checked) {
          pool = pool.filter((item) => item.provider_id !== model.provider_id || item.model !== model.model);
        }
        tiers[tierKey] = pool;
        markConfigDirty();
        updateAdvancedConfigEditor();
      });
      const copy = document.createElement("span");
      const name = document.createElement("strong");
      name.textContent = model.model;
      const provider = document.createElement("small");
      provider.textContent = model.provider_name;
      copy.append(name, provider);
      label.append(input, copy);
      body.appendChild(label);
    }
    return group;
  }

  function renderModelPools() {
    const providers = Array.isArray(state.configDraft?.providers) ? state.configDraft.providers : [];
    const choices = configuredModelChoices();
    const multimodal = choices.filter((choice) => {
      const provider = providers.find((item) => item.id === choice.provider_id);
      return providerModelSupportsMedia(provider, choice.model);
    });
    elements.modelPoolEditor.replaceChildren(
      renderModelPoolList("文本模型池", "active_provider_models", choices),
      renderModelPoolList("多模态模型池", "active_multimodal_provider_models", multimodal),
      renderSubagentTierList("子代理档位池 · cheap（简单任务）", "cheap", choices),
      renderSubagentTierList("子代理档位池 · balanced（普通任务）", "balanced", choices),
      renderSubagentTierList("子代理档位池 · strong（复杂任务）", "strong", choices)
    );
  }

  const PLUGIN_LABELS = {
    weather: "天气", web: "网络搜索", web_images: "图片搜索", deep_research: "深度研究", deep_diagnose: "深度诊断",
    vision: "识图", exchange_rate: "汇率", xuanxue: "玄学", image_generation: "生图", print_image: "打印图片",
    memes: "表情包", knowledge_base: "知识库", archlinux: "Arch Linux", man: "在线手册", moegirl: "萌娘百科",
    hash_codec: "哈希与编解码", calculator: "计算器", package_advisor: "AUR 审查",
    deep_research_linux_game_compatibility: "Linux 游戏兼容", diagnostics: "系统诊断", api_quota: "大模型额度查询", memory: "记忆"
  };

  const SECRET_PLUGIN_PATHS = new Map([
    ["web.tavily_api_keys", "plugins.web.tavily_api_keys"],
    ["web.firecrawl_api_keys", "plugins.web.firecrawl_api_keys"],
    ["web.anysearch_api_keys", "plugins.web.anysearch_api_keys"],
    ["web.exa_api_keys", "plugins.web.exa_api_keys"],
    ["exchange_rate.api_key", "plugins.exchange_rate.api_key"],
    ["image_generation.api_keys", "plugins.image_generation.api_keys"]
  ]);

  const WEB_HIDDEN_PLUGIN_FIELDS = new Set([
    "vision.preview_with_chafa",
    "image_generation.auto_print",
    "print_image.width_percent",
    "print_image.height_percent",
    "memes.width_percent",
    "memes.height_percent",
    "web_images.auto_preview",
    "web_images.preview_count"
  ]);

  function humanizeConfigKey(key) {
    return String(key).replace(/_/g, " ").replace(/\b\w/g, (character) => character.toUpperCase());
  }

  function pluginValueEditor(pluginKey, fieldKey, value) {
    const path = `plugins.${pluginKey}.${fieldKey}`;
    const secretKey = SECRET_PLUGIN_PATHS.get(`${pluginKey}.${fieldKey}`);
    if (secretKey) return secretEditor(humanizeConfigKey(fieldKey), secretKey, { multiline: Array.isArray(value) });
    if (typeof value === "boolean") return booleanConfigField(humanizeConfigKey(fieldKey), path);
    if (typeof value === "number") return textConfigField(humanizeConfigKey(fieldKey), path, { type: "number", integer: Number.isInteger(value), inputType: "number", step: Number.isInteger(value) ? 1 : 0.01 });
    if (typeof value === "string") return textConfigField(humanizeConfigKey(fieldKey), path, { multiline: value.length > 100, rows: 3 });
    return textConfigField(humanizeConfigKey(fieldKey), path, { type: "json", multiline: true, rows: 5 });
  }

  function apiQuotaProviderEditor(providerKey, provider) {
    const details = document.createElement("details");
    details.className = "plugin-subsection";
    const summary = document.createElement("summary");
    summary.textContent = providerKey === "deepseek" ? "DeepSeek" : "OpenRouter";
    const body = document.createElement("div");
    body.className = "plugin-subsection-body";
    const hint = document.createElement("p");
    hint.className = "config-field-hint";
    hint.textContent = providerKey === "deepseek"
      ? "DeepSeek API 余额按 CNY 与 USD 分为两个独立余额池，以下分别显示各币种总余额。"
      : "每个账号配置对应一个 OpenRouter API Key。";
    body.appendChild(hint);
    provider.accounts = Array.isArray(provider.accounts) && provider.accounts.length
      ? provider.accounts
      : [{ id: "account-1", name: "默认账号", api_key: "" }];
    const nextAccountId = () => `account-${globalThis.crypto?.randomUUID?.() || `${Date.now()}-${Math.random().toString(36).slice(2)}`}`;
    const nextAccountName = () => {
      let number = 2;
      while (provider.accounts.some((account) => account.name === `账号 ${number}`)) number += 1;
      return `账号 ${number}`;
    };
    const reindexSecrets = (previousAccounts) => {
      const prefix = `plugins.api_quota.${providerKey}.accounts.`;
      const previous = new Map(previousAccounts.map((account, index) => {
        const key = `${prefix}${index}.api_key`;
        return [account.id || account.name, {
          configured: Boolean(state.secretStates[key]),
          change: state.secretChanges[key]
        }];
      }));
      for (const key of Object.keys(state.secretChanges)) {
        if (key.startsWith(prefix)) delete state.secretChanges[key];
      }
      for (const key of Object.keys(state.secretStates)) {
        if (key.startsWith(prefix)) delete state.secretStates[key];
      }
      provider.accounts.forEach((account, index) => {
        const prior = previous.get(account.id || account.name);
        const key = `${prefix}${index}.api_key`;
        state.secretStates[key] = Boolean(prior?.configured);
        if (prior?.change) state.secretChanges[key] = prior.change;
      });
    };
    const renderAccounts = () => {
      for (const child of Array.from(accountsBody.children)) child.remove();
      provider.accounts.forEach((account, index) => {
        const accountDetails = document.createElement("details");
        accountDetails.className = "quota-account";
        accountDetails.open = true;
        const accountSummary = document.createElement("summary");
        const accountTitle = document.createElement("span");
        accountTitle.textContent = account.name || `账号 ${index + 1}`;
        const remove = actionButton("删除", "text-button danger-text");
        remove.addEventListener("click", (event) => {
          event.preventDefault();
          if (!window.confirm(`删除账号配置“${account.name || `账号 ${index + 1}`}”？`)) return;
          const previousAccounts = provider.accounts.map((item) => ({ ...item }));
          const deletingOnlyAccount = provider.accounts.length === 1;
          if (deletingOnlyAccount) {
            provider.accounts[0] = { id: provider.accounts[0].id || "account-1", name: "默认账号", api_key: "" };
          } else {
            provider.accounts.splice(index, 1);
          }
          reindexSecrets(previousAccounts);
          if (deletingOnlyAccount) {
            const key = `plugins.api_quota.${providerKey}.accounts.0.api_key`;
            state.secretStates[key] = false;
            state.secretChanges[key] = { action: "clear" };
          }
          markConfigDirty();
          renderAccounts();
        });
        accountSummary.append(accountTitle, remove);
        const accountBody = document.createElement("div");
        accountBody.className = "quota-account-body";
        accountBody.appendChild(textConfigField("账号名称", `plugins.api_quota.${providerKey}.accounts.${index}.name`, { value: account.name || `账号 ${index + 1}` }));
        accountBody.appendChild(secretEditor("API Key", `plugins.api_quota.${providerKey}.accounts.${index}.api_key`));
        accountDetails.append(accountSummary, accountBody);
        accountsBody.appendChild(accountDetails);
      });
    };
    const accountsBody = document.createElement("div");
    accountsBody.className = "quota-accounts";
    body.appendChild(accountsBody);
    const add = actionButton("新建账号", "text-button");
    add.addEventListener("click", () => {
      if (provider.accounts.length >= 32) {
        showToast("每个平台最多配置 32 个账号", "error");
        return;
      }
      const previousAccounts = provider.accounts.map((item) => ({ ...item }));
      provider.accounts.push({ id: nextAccountId(), name: nextAccountName(), api_key: "" });
      reindexSecrets(previousAccounts);
      markConfigDirty();
      renderAccounts();
    });
    body.appendChild(add);
    renderAccounts();
    details.append(summary, body);
    return details;
  }

  function remapApiQuotaSecrets(previousConfig, nextConfig) {
    for (const providerKey of ["deepseek", "openrouter"]) {
      const prefix = `plugins.api_quota.${providerKey}.accounts.`;
      const previousAccounts = previousConfig?.plugins?.api_quota?.[providerKey]?.accounts || [];
      const saved = new Map(previousAccounts.map((account, index) => {
        const key = `${prefix}${index}.api_key`;
        return [account.id, {
          configured: Boolean(state.secretStates[key]),
          change: state.secretChanges[key]
        }];
      }).filter(([id]) => id));
      for (const key of Object.keys(state.secretStates)) {
        if (key.startsWith(prefix)) delete state.secretStates[key];
      }
      for (const key of Object.keys(state.secretChanges)) {
        if (key.startsWith(prefix)) delete state.secretChanges[key];
      }
      const nextAccounts = nextConfig?.plugins?.api_quota?.[providerKey]?.accounts || [];
      nextAccounts.forEach((account, index) => {
        const prior = saved.get(account.id);
        const key = `${prefix}${index}.api_key`;
        state.secretStates[key] = Boolean(prior?.configured);
        if (prior?.change) state.secretChanges[key] = prior.change;
      });
    }
  }

  function renderPlugins() {
    elements.pluginEditor.replaceChildren();
    for (const [pluginKey, plugin] of Object.entries(state.configDraft?.plugins || {})) {
      if (pluginKey === "memory" || pluginKey === "print_image") continue;
      const details = document.createElement("details");
      details.className = "plugin-card";
      const summary = document.createElement("summary");
      const copy = document.createElement("span");
      const title = document.createElement("strong");
      title.textContent = PLUGIN_LABELS[pluginKey] || humanizeConfigKey(pluginKey);
      const technical = document.createElement("small");
      technical.textContent = pluginKey;
      copy.append(title, technical);
      const badge = document.createElement("span");
      badge.className = `plugin-state${plugin?.enabled ? " is-enabled" : ""}`;
      badge.textContent = plugin?.enabled ? "启用" : "禁用";
      summary.append(copy, badge);
      const body = document.createElement("div");
      body.className = "plugin-card-body";
      for (const [fieldKey, value] of Object.entries(plugin || {})) {
        if (WEB_HIDDEN_PLUGIN_FIELDS.has(`${pluginKey}.${fieldKey}`)) continue;
        if (pluginKey === "api_quota" && (fieldKey === "deepseek" || fieldKey === "openrouter")) {
          body.appendChild(apiQuotaProviderEditor(fieldKey, value));
          continue;
        }
        body.appendChild(pluginValueEditor(pluginKey, fieldKey, value));
      }
      details.append(summary, body);
      elements.pluginEditor.appendChild(details);
    }
  }

  function normalizedDocumentName(name) {
    const trimmed = String(name || "").trim().replace(/[\\/]/g, "-").replace(/\.md$/i, "");
    return trimmed ? `${trimmed}.md` : "";
  }

  function personaTextField(promptDocument, key, label, placeholder) {
    const input = document.createElement("input");
    input.className = "config-input";
    input.type = "text";
    input.maxLength = 200;
    input.placeholder = placeholder;
    input.value = String(promptDocument[key] || "");
    input.addEventListener("input", () => {
      promptDocument[key] = input.value.trim() || null;
      markConfigDirty();
    });
    return configField(label, input);
  }

  function personaImageField(promptDocument, key, label, fallbackUrl) {
    const pathInput = document.createElement("input");
    pathInput.className = "config-input";
    pathInput.type = "text";
    pathInput.placeholder = "";
    pathInput.value = String(promptDocument[key] || "");
    const picker = document.createElement("input");
    picker.type = "file";
    picker.accept = "image/png,image/jpeg,image/webp,image/gif,image/bmp";
    picker.hidden = true;
    const pickButton = actionButton("", "icon-button");
    pickButton.title = `选择${label.replace(/^自定义/, "")}`;
    pickButton.setAttribute("aria-label", pickButton.title);
    pickButton.appendChild(makeIconSlot("folder"));
    pickButton.addEventListener("click", () => picker.click());
    const preview = document.createElement("img");
    preview.className = `persona-avatar-preview${key === "board_image_path" ? " persona-board-preview" : ""}`;
    preview.alt = "";
    preview.setAttribute("aria-hidden", "true");
    const showStoredPreview = () => {
      preview.classList.remove("is-missing");
      preview.src = promptDocument[key]
        ? `/api/persona/avatar?path=${encodeURIComponent(promptDocument[key])}`
        : fallbackUrl || "";
      if (!promptDocument[key] && !fallbackUrl) {
        preview.removeAttribute("src");
        preview.classList.add("is-missing");
      }
    };
    preview.addEventListener("error", () => {
      preview.removeAttribute("src");
      preview.classList.add("is-missing");
    });
    showStoredPreview();
    pathInput.addEventListener("input", () => {
      promptDocument[key] = pathInput.value.trim() || null;
      showStoredPreview();
      markConfigDirty();
    });
    picker.addEventListener("change", async () => {
      const file = picker.files?.[0];
      if (!file) return;
      if (file.size > 8 * 1024 * 1024) return showToast("图片不能超过 8 MiB", "error");
      if (preview.src && preview.src.startsWith("blob:")) URL.revokeObjectURL(preview.src);
      preview.src = URL.createObjectURL(file);
      preview.classList.remove("is-missing");
      pickButton.disabled = true;
      try {
        const response = await apiRequest("/api/persona/assets", {
          method: "POST",
          headers: { "Content-Type": file.type || "application/octet-stream" },
          body: file
        });
        const result = await response.json();
        promptDocument[key] = result.path;
        pathInput.value = result.path;
        preview.src = result.preview_url;
        markConfigDirty();
      } catch (error) {
        showToast(error.message || "图片上传失败", "error");
      } finally {
        pickButton.disabled = false;
        picker.value = "";
      }
    });
    const row = document.createElement("div");
    row.className = "avatar-path-row";
    row.append(pathInput, pickButton, preview, picker);
    return configField(label, row);
  }

  function renderPromptCollection(kind, titleText, activePath) {
    const documents = state.promptDraft[kind];
    const group = configGroup(titleText);
    const body = group.querySelector(".config-group-body");
    const active = document.createElement("select");
    active.className = "config-input";
    const defaultOption = document.createElement("option");
    defaultOption.value = "";
    defaultOption.textContent = kind === "personas" ? "Miyu 默认人格" : "不使用用户身份";
    active.appendChild(defaultOption);
    for (const promptDocument of documents) {
      const option = document.createElement("option");
      option.value = promptDocument.name;
      option.textContent = promptDocument.name.replace(/\.md$/i, "");
      active.appendChild(option);
    }
    active.value = String(configValue(activePath, ""));
    active.addEventListener("change", () => { setConfigValue(activePath, active.value); renderPromptEditor(); updateAdvancedConfigEditor(); });
    body.appendChild(configField("当前使用", active));
    const selected = documents.find((document) => document.name === active.value);
    for (const [index, promptDocument] of documents.entries()) {
      if (promptDocument !== selected) continue;
      const card = document.createElement("section");
      card.className = "prompt-document";
      const header = document.createElement("header");
      const name = document.createElement("input");
      name.className = "config-input";
      name.value = promptDocument.name.replace(/\.md$/i, "");
      name.setAttribute("aria-label", `${titleText}名称`);
      const remove = actionButton("删除", "text-button danger-text");
      remove.addEventListener("click", () => {
        const wasActive = configValue(activePath, "") === promptDocument.name;
        documents.splice(index, 1);
        if (wasActive) setConfigValue(activePath, "");
        markConfigDirty();
        renderPromptEditor();
        updateAdvancedConfigEditor();
      });
      header.append(configField("名称", name), remove);
      const content = document.createElement("textarea");
      content.className = "config-input prompt-content";
      content.rows = 10;
      content.value = promptDocument.content;
      content.setAttribute("aria-label", `${titleText}内容`);
      name.addEventListener("input", () => {
        const previous = promptDocument.name;
        promptDocument.name = normalizedDocumentName(name.value);
        if (configValue(activePath, "") === previous) setConfigValue(activePath, promptDocument.name);
        markConfigDirty();
        updateAdvancedConfigEditor();
      });
      content.addEventListener("input", () => { promptDocument.content = content.value; markConfigDirty(); });
      card.append(header, configField("内容", content));
      if (kind === "personas") {
        card.append(
          personaImageField(promptDocument, "avatar_path", "自定义头像图片", null),
          personaImageField(promptDocument, "board_image_path", "自定义看板图片", null),
          personaTextField(promptDocument, "board_title", "自定义看板大字", DEFAULT_BOARD_TITLE),
          personaTextField(promptDocument, "board_subtitle", "自定义看板小字", DEFAULT_BOARD_SUBTITLE)
        );
        const starterFields = document.createElement("div");
        starterFields.className = "persona-starter-fields";
        const values = Array.isArray(promptDocument.starter_prompts)
          ? DEFAULT_STARTER_PROMPTS.map((_, index) => String(promptDocument.starter_prompts[index] || ""))
          : DEFAULT_STARTER_PROMPTS.map(() => "");
        values.forEach((value, promptIndex) => {
          const input = document.createElement("input");
          input.className = "config-input";
          input.type = "text";
          input.maxLength = 200;
          input.value = value;
          input.placeholder = DEFAULT_STARTER_PROMPTS[promptIndex];
          input.setAttribute("aria-label", `预设问题 ${promptIndex + 1}`);
          input.addEventListener("input", () => {
            values[promptIndex] = input.value;
            promptDocument.starter_prompts = values.some((item) => item.trim()) ? [...values] : null;
            markConfigDirty();
          });
          starterFields.appendChild(input);
        });
        card.appendChild(configField("自定义预设问题", starterFields));
      }
      body.appendChild(card);
    }
    const add = actionButton("添加", "secondary-button compact-button");
    add.addEventListener("click", () => {
      const base = kind === "personas" ? "新建人格" : "新建身份";
      let name = `${base}.md`;
      let suffix = 2;
      while (documents.some((document) => document.name === name)) name = `${base} ${suffix++}.md`;
      documents.push({ name, content: "", avatar_path: null, original_name: null });
      setConfigValue(activePath, name);
      markConfigDirty();
      renderPromptEditor();
    });
    body.appendChild(add);
    return group;
  }

  function renderPromptEditor() {
    elements.promptEditor.replaceChildren(
      renderPromptCollection("personas", "AI 人格", "prompt.active_persona"),
      renderPromptCollection("identities", "用户身份", "prompt.active_identity")
    );
  }

  function renderConfigEditors() {
    if (!state.configLoaded || !state.configDraft) return;
    state.invalidConfigFields.clear();
    renderGeneralConfig();
    renderProviders();
    renderModelPools();
    renderPlugins();
    renderPromptEditor();
    updateAdvancedConfigEditor();
    updateSettingsControls();
  }

  function mapServerSecretStates(payload) {
    const providers = state.configDraft?.providers || [];
    state.providerSecretStates = providers.map((_, index) => Boolean(payload[`providers.${index}.api_key`]));
    const states = { ...payload };
    state.secretStates = states;
    refreshProviderSecretStates();
    return states;
  }

  // 配置文件会省略未修改的平台默认值；草稿仍需补齐真实语义，
  // 以免 WebUI 保存其他设置时覆盖通讯平台的默认策略。
  function ensurePlatformDefaults(draft) {
    if (!draft || typeof draft !== "object") return;
    draft.platforms = Object.assign({
      command_prefix: "/",
      commands: {}
    }, draft.platforms);
    const qq = Object.assign({
      enabled: false,
      reverse_ws_port: 8300,
      access_token: "",
      admin_users: [],
      allow_non_admin_host_tools: false,
      user_identification: true,
      show_group_name: true,
      conversations: [],
      plugins: {},
      asset_base_url: "",
      max_reply_chars: 3000,
    }, draft.platforms.qq);
    qq.private_chats = Object.assign({
      whitelist: [],
      allow_non_whitelist: true,
      non_whitelist_rate_limit: { max_messages: 2, window_seconds: 600 }
    }, qq.private_chats);
    qq.group_chats = Object.assign({
      whitelist: [],
      trigger_keywords: [],
      whitelist_rate_limit: { max_messages: 30, window_seconds: 60 },
      allow_non_whitelist: true,
      non_whitelist_rate_limit: { max_messages: 2, window_seconds: 600 }
    }, qq.group_chats);
    draft.platforms.qq = qq;
  }

  function applyConfigPayload(payload) {
    state.configDraft = deepClone(payload?.config || {});
    ensurePlatformDefaults(state.configDraft);
    state.configOriginal = deepClone(payload?.config || {});
    state.promptDraft = deepClone(payload?.prompts || { personas: [], identities: [] });
    state.promptOriginal = deepClone(payload?.prompts || { personas: [], identities: [] });
    state.secretChanges = {};
    mapServerSecretStates(payload?.secret_states || {});
    state.configDirty = false;
    state.configLoaded = true;
    state.invalidConfigFields.clear();
    if (Array.isArray(payload?.models)) state.models = payload.models;
    state.configMultimodalModels = Array.isArray(payload?.multimodal_models) ? payload.multimodal_models : [];
    const providersById = new Map(
      (Array.isArray(state.configDraft?.providers) ? state.configDraft.providers : [])
        .map((provider) => [String(provider?.id || ""), provider])
    );
    state.configInferredImageModels = state.configMultimodalModels.filter((model) => {
      const provider = providersById.get(String(model?.provider_id || ""));
      const declared = provider?.model_modalities;
      return !(declared && typeof declared === "object"
        && Object.prototype.hasOwnProperty.call(declared, String(model?.model || "")));
    });
    if (payload?.display && typeof payload.display === "object") state.display = payload.display;
    if (payload?.context && typeof payload.context === "object") state.context = payload.context;
    if (payload?.persona) applyPersona(payload.persona);
    renderConfigEditors();
    renderModelMenu();
    updateContext();
  }

  async function loadConfigDraft() {
    if (state.configLoading || state.configSaving) return;
    if (state.configDirty && !window.confirm("放弃尚未保存的配置修改并重新载入？")) return;
    state.configLoading = true;
    updateSettingsControls();
    try {
      const response = await apiRequest("/api/config");
      applyConfigPayload(await response.json());
    } catch (error) {
      showToast(error.message || "配置载入失败", "error");
      elements.settingsStatus.textContent = error.message || "配置载入失败";
    } finally {
      state.configLoading = false;
      updateSettingsControls();
    }
  }

  function promptStateChanged() {
    if (!state.configOriginal || !state.promptOriginal) return false;
    const promptKeys = ["prompt", "system_prompt_file", "system_prompt"];
    const current = Object.fromEntries(promptKeys.map((key) => [key, state.configDraft?.[key]]));
    const original = Object.fromEntries(promptKeys.map((key) => [key, state.configOriginal?.[key]]));
    const withoutPersonaMetadata = (documents) => Object.fromEntries(
      Object.entries(documents || {}).map(([kind, items]) => [
        kind,
        (Array.isArray(items) ? items : []).map(({
          avatar_path: _avatarPath,
          board_image_path: _BoardImagePath,
          board_title: _BoardTitle,
          board_subtitle: _BoardSubtitle,
          starter_prompts: _StarterPrompts,
          ...document
        }) => document)
      ])
    );
    return JSON.stringify(current) !== JSON.stringify(original)
      || JSON.stringify(withoutPersonaMetadata(state.promptDraft)) !== JSON.stringify(withoutPersonaMetadata(state.promptOriginal));
  }

  function buildSecretMutations() {
    return { ...state.secretChanges };
  }

  async function saveConfigDraft() {
    if (!state.configLoaded || state.configSaving || state.configLoading || conversationRunning() || state.invalidConfigFields.size) return;
    const personaChanged = String(state.configDraft?.prompt?.active_persona || "")
      !== String(state.configOriginal?.prompt?.active_persona || "");
    state.configSaving = true;
    state.adminBusy = true;
    updateSettingsControls();
    updateControlState();
    try {
      const response = await apiRequest("/api/config", {
        method: "PUT",
        body: JSON.stringify({
          config: state.configDraft,
          secrets: buildSecretMutations(),
          prompts: state.promptDraft,
          reset_conversation: false
        })
      });
      applyConfigPayload(await response.json());
      if (personaChanged) await loadBootstrap();
      showToast("配置已保存");
    } catch (error) {
      showToast(error.message || "配置保存失败", "error");
      elements.settingsStatus.textContent = error.message || "配置保存失败";
    } finally {
      state.configSaving = false;
      state.adminBusy = false;
      updateSettingsControls();
      updateControlState();
    }
  }

  function applyAdvancedConfig() {
    try {
      const parsed = JSON.parse(elements.advancedConfigEditor.value);
      if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) throw new Error("配置必须是 JSON 对象");
      const oldSecretStates = new Map((state.configDraft?.providers || []).map((provider, index) => [String(provider?.id || ""), Boolean(state.providerSecretStates[index])]));
      remapApiQuotaSecrets(state.configDraft, parsed);
      state.configDraft = parsed;
      ensurePlatformDefaults(state.configDraft);
      state.providerSecretStates = (Array.isArray(parsed.providers) ? parsed.providers : []).map((provider) => oldSecretStates.get(String(provider?.id || "")) || false);
      refreshProviderSecretStates();
      clearProviderSecretChanges();
      markConfigDirty();
      renderConfigEditors();
      showToast("完整配置已应用到草稿");
    } catch (error) {
      showToast(error.message || "JSON 无效", "error");
    }
  }

  async function readErrorMessage(response) {
    try {
      const payload = await response.json();
      const message = payload?.error?.message;
      if (typeof message === "string" && message.trim()) return message.trim();
    } catch (_) {
      // Fall through to an HTTP status message.
    }
    return `请求失败 (${response.status})`;
  }

  async function apiRequest(path, options = {}) {
    const headers = new Headers(options.headers || {});
    headers.set("Accept", "application/json");
    if (options.body != null && !headers.has("Content-Type")) headers.set("Content-Type", "application/json");
    let response;
    try {
      response = await fetch(path, { ...options, headers, credentials: "same-origin" });
    } catch (_) {
      throw new ApiError("无法连接 Miyu WebUI", 0);
    }
    if (!response.ok) throw new ApiError(await readErrorMessage(response), response.status);
    return response;
  }

  function qqHistoryQuery() {
    return new URLSearchParams({
      account_id: elements.qqHistoryAccount.value.trim(),
      group_id: elements.qqHistoryGroup.value.trim()
    });
  }

  function qqHistoryButton(label, className, onClick) {
    const button = document.createElement("button");
    button.type = "button";
    button.className = className;
    button.textContent = label;
    button.addEventListener("click", onClick);
    return button;
  }

  function renderQqHistory(data) {
    const offenderValue = data?.offender_history ?? data?.offenders;
    const offenders = offenderValue && typeof offenderValue === "object" && !Array.isArray(offenderValue) ? offenderValue : {};
    const kickValue = data?.kick_history ?? data?.kicks;
    const kicks = Array.isArray(kickValue) ? kickValue : [];
    const output = elements.qqHistoryOutput;
    output.replaceChildren();
    const heading = document.createElement("div");
    heading.className = "qq-history-summary";
    heading.textContent = `违规者 ${formatInteger(Object.keys(offenders).length)} 人 · 踢人 ${formatInteger(kicks.length)} 条`;
    output.appendChild(heading);

    const offenderSection = document.createElement("section");
    offenderSection.className = "qq-history-list";
    const offenderTitle = document.createElement("h3");
    offenderTitle.textContent = "违规者统计";
    offenderSection.appendChild(offenderTitle);
    for (const [userId, record] of Object.entries(offenders)) {
      const row = document.createElement("div");
      row.className = "qq-history-row";
      const text = document.createElement("span");
      text.textContent = `${record?.user_name || "未知用户"} (${userId}) · ${formatInteger(record?.ban_count)} 次 · ${record?.last_reason || "无原因"}`;
      const remove = qqHistoryButton("删除", "text-button danger-text", async () => {
        if (!window.confirm(`删除 ${userId} 的违规记录？`)) return;
        try {
          await apiRequest(`/api/qq-group-management/offenders/${encodeURIComponent(userId)}?${qqHistoryQuery()}`, { method: "DELETE" });
          await loadQqHistory();
        } catch (error) { showToast(error.message, "error"); }
      });
      row.append(text, remove);
      offenderSection.appendChild(row);
    }
    if (!Object.keys(offenders).length) offenderSection.appendChild(qqHistoryEmpty("暂无违规者记录"));
    output.appendChild(offenderSection);

    const kickSection = document.createElement("section");
    kickSection.className = "qq-history-list";
    const kickHeader = document.createElement("div");
    kickHeader.className = "qq-history-list-heading";
    const kickTitle = document.createElement("h3");
    kickTitle.textContent = "踢人历史";
    kickHeader.appendChild(kickTitle);
    if (kicks.length) kickHeader.appendChild(qqHistoryButton("清空", "text-button danger-text", () => clearQqHistory("kicks")));
    kickSection.appendChild(kickHeader);
    for (const record of kicks.slice().reverse()) {
      const row = document.createElement("div");
      row.className = "qq-history-row qq-history-kick";
      const kickedAt = typeof record?.kicked_at === "number" ? record.kicked_at * 1000 : record?.kicked_at;
      row.textContent = `${record?.user_name || "未知用户"} (${record?.user_id || "--"}) · ${record?.reason || "无原因"} · ${formatDateTime(kickedAt)}`;
      kickSection.appendChild(row);
    }
    if (!kicks.length) kickSection.appendChild(qqHistoryEmpty("暂无踢人记录"));
    output.appendChild(kickSection);
    if (Object.keys(offenders).length) {
      const clear = qqHistoryButton("清空违规者", "text-button danger-text", () => clearQqHistory("offenders"));
      offenderTitle.appendChild(clear);
      offenderTitle.className = "qq-history-list-heading";
    }
    output.hidden = false;
  }

  function qqHistoryEmpty(text) {
    const empty = document.createElement("p");
    empty.className = "settings-empty";
    empty.textContent = text;
    return empty;
  }

  async function loadQqHistory() {
    const account = elements.qqHistoryAccount.value.trim();
    const group = elements.qqHistoryGroup.value.trim();
    if (!/^\d{5,12}$/.test(account) || !/^\d{5,12}$/.test(group)) {
      showToast("请输入有效的 bot QQ 和群号", "error");
      return;
    }
    elements.qqHistoryStatus.textContent = "正在加载记录...";
    try {
      const response = await apiRequest(`/api/qq-group-management/history?${qqHistoryQuery()}`);
      const data = await response.json();
      const accounts = Array.isArray(data.connected_accounts) ? data.connected_accounts : [];
      elements.qqHistoryStatus.textContent = accounts.length ? `当前连接账户：${accounts.join("、")}` : "当前没有在线连接账户";
      renderQqHistory(data);
    } catch (error) {
      elements.qqHistoryStatus.textContent = "";
      showToast(error.message, "error");
    }
  }

  async function clearQqHistory(kind) {
    const title = kind === "offenders" ? "违规者记录" : "踢人记录";
    if (!window.confirm(`清空全部${title}？此操作无法撤销。`)) return;
    try {
      await apiRequest("/api/qq-group-management/history/clear", {
        method: "POST",
        body: JSON.stringify({ account_id: elements.qqHistoryAccount.value.trim(), group_id: elements.qqHistoryGroup.value.trim(), kind })
      });
      await loadQqHistory();
    } catch (error) { showToast(error.message, "error"); }
  }

  function asFiniteNumber(value, fallback = 0) {
    const number = Number(value);
    return Number.isFinite(number) ? number : fallback;
  }

  function formatInteger(value) {
    const number = Math.max(0, asFiniteNumber(value));
    try {
      return new Intl.NumberFormat("zh-CN", { maximumFractionDigits: 0 }).format(number);
    } catch (_) {
      return String(Math.round(number));
    }
  }

  // 缓存命中率只以输入为分母：输出 token 要到下一轮才进入输入，把它算进
  // 分母会让同样的缓存效果随回复变长而显得越来越差。三家供应商的用量字段
  // 也都是这么定义的（DeepSeek 直接把 prompt 劈成 hit+miss）。
  function cacheSuffix(cached, prompt) {
    const hit = asFiniteNumber(cached, 0);
    const total = asFiniteNumber(prompt, 0);
    if (hit <= 0 || total <= 0) return "";
    return `（C${Math.min(100, Math.round((hit / total) * 100))}%）`;
  }

  function formatUsageMeta({ turnTotal, turnPrompt, turnCached, estimated, cumulative, cumulativePrompt, cumulativeCached }) {
    const parts = [];
    if (asFiniteNumber(turnTotal) > 0) {
      parts.push(`本轮${estimated ? "约 " : " "}${formatTokens(turnTotal)}${cacheSuffix(turnCached, turnPrompt)}`);
    }
    if (asFiniteNumber(cumulative) > 0) {
      parts.push(`累计 ${formatTokens(cumulative)}${cacheSuffix(cumulativeCached, cumulativePrompt)}`);
    }
    return parts.join(" · ");
  }

  function formatTokens(value) {
    const number = Math.max(0, asFiniteNumber(value));
    if (number < 1000) return formatInteger(number);
    const useMillions = number >= 1_000_000;
    const amount = number / (useMillions ? 1_000_000 : 1000);
    const digits = amount >= 100 ? 0 : amount >= 10 ? 1 : 1;
    const suffix = useMillions ? "M" : "k";
    try {
      return `${new Intl.NumberFormat("zh-CN", { maximumFractionDigits: digits }).format(amount)}${suffix}`;
    } catch (_) {
      return `${amount.toFixed(digits)}${suffix}`;
    }
  }

  function parseDate(value) {
    if (value == null || value === "") return null;
    const date = new Date(value);
    return Number.isNaN(date.getTime()) ? null : date;
  }

  function formatTime(value) {
    const date = parseDate(value);
    if (!date) return "";
    try {
      return new Intl.DateTimeFormat("zh-CN", { hour: "2-digit", minute: "2-digit", hour12: false }).format(date);
    } catch (_) {
      return date.toLocaleTimeString?.() || "";
    }
  }

  function formatDateTime(value) {
    const date = parseDate(value);
    if (!date) return "";
    try {
      return new Intl.DateTimeFormat("zh-CN", {
        year: "numeric",
        month: "2-digit",
        day: "2-digit",
        hour: "2-digit",
        minute: "2-digit",
        hour12: false
      }).format(date);
    } catch (_) {
      return date.toLocaleString?.() || "";
    }
  }

  function formatRelativeTime(value) {
    const date = parseDate(value);
    if (!date) return "";
    const difference = Date.now() - date.getTime();
    if (difference >= 0 && difference < 60_000) return "刚刚";
    if (difference >= 0 && difference < 3_600_000) return `${Math.max(1, Math.floor(difference / 60_000))} 分钟前`;
    const now = new Date();
    if (date.toDateString() === now.toDateString()) return formatTime(date);
    try {
      return new Intl.DateTimeFormat("zh-CN", { month: "numeric", day: "numeric" }).format(date);
    } catch (_) {
      return date.toLocaleDateString?.() || "";
    }
  }

  function dayKey(value) {
    const date = parseDate(value);
    if (!date) return "unknown";
    return `${date.getFullYear()}-${date.getMonth()}-${date.getDate()}`;
  }

  function formatDayLabel(value) {
    const date = parseDate(value);
    if (!date) return "较早";
    const today = new Date();
    const yesterday = new Date(today);
    yesterday.setDate(today.getDate() - 1);
    if (date.toDateString() === today.toDateString()) return "今天";
    if (date.toDateString() === yesterday.toDateString()) return "昨天";
    try {
      return new Intl.DateTimeFormat("zh-CN", { year: "numeric", month: "long", day: "numeric" }).format(date);
    } catch (_) {
      return date.toLocaleDateString?.() || "较早";
    }
  }

  function firstLine(value) {
    return String(value || "").split(/\r?\n/, 1)[0].trim();
  }

  function modelMark(model) {
    const source = String(model?.provider_name || model?.provider_id || model?.model || "").trim();
    if (!source) return "--";
    const words = source.split(/[\s._/-]+/).filter(Boolean);
    const mark = words.length > 1 ? `${words[0][0] || ""}${words[1][0] || ""}` : source.slice(0, 2);
    return mark.toLocaleUpperCase("en-US");
  }

  function modelKey(model) {
    return JSON.stringify([String(model?.provider_id || ""), String(model?.model || "")]);
  }

  function effectiveUsageTotal(usage) {
    if (!usage || typeof usage !== "object") return 0;
    const explicit = asFiniteNumber(usage.total_tokens, 0);
    return explicit > 0 ? explicit : asFiniteNumber(usage.prompt_tokens, 0) + asFiniteNumber(usage.completion_tokens, 0);
  }

  function setConnectionStatus(status) {
    state.connection = status;
    const definitions = {
      online: { sidebar: "在线", className: "" },
      connecting: { sidebar: "重连中", className: "is-connecting" },
      offline: { sidebar: "离线", className: "is-offline" },
      blocked: { sidebar: "未授权", className: "is-blocked" }
    };
    const selected = definitions[status] || definitions.connecting;
    elements.sidebarConnectionStatus.textContent = selected.sidebar;
    elements.sidebarStatusDot.classList.remove("is-connecting", "is-offline", "is-blocked");
    if (selected.className) elements.sidebarStatusDot.classList.add(selected.className);
  }

  function updateContext() {
    const tokens = Math.max(0, asFiniteNumber(state.context?.tokens));
    const windowSize = state.context?.window == null ? null : Math.max(0, asFiniteNumber(state.context.window));
    elements.contextNumbers.textContent = windowSize ? `${formatTokens(tokens)} / ${formatTokens(windowSize)}` : `${formatTokens(tokens)} / --`;
    const percent = windowSize > 0 ? Math.min(100, Math.max(0, (tokens / windowSize) * 100)) : 0;
    elements.contextBar.style.width = `${percent}%`;
    elements.contextTrack.setAttribute("aria-valuenow", String(Math.round(percent)));
    elements.contextTrack.setAttribute("aria-label", windowSize ? `上下文使用 ${Math.round(percent)}%` : `上下文 ${formatInteger(tokens)} tokens`);
    elements.contextTrack.classList.toggle("is-high", percent >= 75 && percent < 90);
    elements.contextTrack.classList.toggle("is-critical", percent >= 90);
  }

  function updateRuntimeUsage() {}

  function updateCapabilities() {
    const values = [
      ["会话", state.capabilities?.multi_conversation ? "多会话" : "当前单一对话"],
      ["附件", state.capabilities?.attachments ? "可用" : "不可用"],
      ["消息队列", state.capabilities?.queue ? "可用" : "不可用"]
    ];
    elements.capabilityList.replaceChildren();
    for (const [name, value] of values) {
      const row = document.createElement("div");
      const term = document.createElement("dt");
      const description = document.createElement("dd");
      term.textContent = name;
      description.textContent = value;
      row.append(term, description);
      elements.capabilityList.appendChild(row);
    }
  }

  function activeModels() {
    return state.models.filter((model) => model?.active);
  }

  function normalizeModelOverride(value) {
    if (!Array.isArray(value)) return null;
    const models = value
      .map((item) => ({ provider_id: String(item?.provider_id || ""), model: String(item?.model || "") }))
      .filter((item) => item.provider_id && item.model);
    return models.length ? models : null;
  }

  function viewSessionModelOverride() {
    return state.viewSessionId && state.sessionModelOverrideFor === state.viewSessionId
      ? state.sessionModelOverride
      : null;
  }

  function describeOverrideModel(entry) {
    const key = modelKey(entry);
    return state.models.find((model) => modelKey(model) === key) || entry;
  }

  function setSessionModelOverride(sessionId, override) {
    state.sessionModelOverrideFor = String(sessionId || "");
    state.sessionModelOverride = normalizeModelOverride(override);
    updateCurrentModelDisplay();
    if (elements.modelMenu.hidden || state.modelSelectionSubmitting) return;
    // 菜单开着且用户尚未改动暂存选择时，同步为最新覆盖状态。
    if (!state.modelMenuTouched && state.stagedModelKeys instanceof Set) {
      const fresh = viewSessionModelOverride();
      const freshFollow = !fresh;
      const freshKeys = new Set((fresh || []).map(modelKey));
      const unchanged = state.stagedFollowGlobal === freshFollow
        && state.stagedModelKeys.size === freshKeys.size
        && [...freshKeys].every((key) => state.stagedModelKeys.has(key));
      if (!unchanged) {
        const hadFocus = elements.modelMenu.contains(document.activeElement);
        resetModelMenuStaging();
        renderModelMenu();
        if (hadFocus) {
          const focusTarget = elements.modelMenu.querySelector(".model-menu-item.selected:not(:disabled)")
            || elements.modelMenu.querySelector(".model-menu-item:not(:disabled)");
          focusTarget?.focus();
        }
        return;
      }
    }
    updateModelMenuState();
  }

  async function refreshSessionModelOverride(sessionId = state.viewSessionId) {
    const target = String(sessionId || "");
    const token = ++state.sessionModelOverrideToken;
    if (!target) {
      setSessionModelOverride("", null);
      return;
    }
    try {
      const response = await apiRequest(`/api/sessions/${encodeURIComponent(target)}/models`);
      const payload = await response.json();
      if (token !== state.sessionModelOverrideToken || state.viewSessionId !== target) return;
      setSessionModelOverride(target, payload?.model_override);
    } catch (_) {
      // 静默失败：顶栏回退显示全局池，下次打开菜单会再次刷新。
    }
  }

  function updateCurrentModelDisplay() {
    // 设置页摘要始终反映全局激活池。
    const active = activeModels();
    if (active.length === 0) {
      elements.settingsModelMark.textContent = "--";
      elements.settingsModelName.textContent = state.models.length ? "未选择模型" : "未配置模型";
      elements.settingsModelProvider.textContent = "--";
    } else if (active.length > 1) {
      elements.settingsModelMark.textContent = "MX";
      elements.settingsModelName.textContent = "混合模型";
      elements.settingsModelProvider.textContent = `${active.length} 个活动端点`;
    } else {
      elements.settingsModelMark.textContent = modelMark(active[0]);
      elements.settingsModelName.textContent = String(active[0].model || "");
      elements.settingsModelProvider.textContent = String(active[0].provider_name || active[0].provider_id || "");
    }

    // 顶栏反映当前会话生效的模型池：有覆盖显示覆盖，否则跟随全局。
    const override = viewSessionModelOverride();
    const pool = override ? override.map(describeOverrideModel) : active;
    const scope = override ? "本会话固定" : "跟随全局";
    if (pool.length === 0) {
      elements.modelMark.textContent = "--";
      elements.modelLabel.textContent = state.models.length ? "未选择模型" : "未配置模型";
      elements.modelLabel.title = `${elements.modelLabel.textContent}（${scope}）`;
      return;
    }
    if (pool.length > 1) {
      const title = pool.map((model) => `${model.provider_name || model.provider_id || ""} · ${model.model || ""}`).join("\n");
      elements.modelMark.textContent = "MX";
      elements.modelLabel.textContent = `混合模型 · ${pool.length}`;
      elements.modelLabel.title = `${scope}\n${title}`;
      return;
    }
    const selected = pool[0];
    elements.modelMark.textContent = modelMark(selected);
    elements.modelLabel.textContent = String(selected.model || "");
    elements.modelLabel.title = `${selected.provider_name || selected.provider_id || ""} · ${selected.model || ""}（${scope}）`;
  }

  function refreshLiveEndpointVisibility() {
    for (const live of state.liveRuns.values()) {
      if (!live.endpoint) continue;
      const values = [live.providerId, live.model].map((value) => String(value || "").trim()).filter(Boolean);
      live.endpoint.hidden = !state.display?.show_mixed_model_endpoint || values.length === 0;
    }
  }

  function resetModelMenuStaging() {
    const override = viewSessionModelOverride();
    state.stagedFollowGlobal = !override;
    state.stagedModelKeys = new Set((override || []).map(modelKey));
    state.modelMenuTouched = false;
    state.modelMenuError = "";
  }

  function modelMenuStaging() {
    if (state.stagedModelKeys instanceof Set) {
      return { follow: state.stagedFollowGlobal, keys: state.stagedModelKeys };
    }
    const override = viewSessionModelOverride();
    return { follow: !override, keys: new Set((override || []).map(modelKey)) };
  }

  function renderModelMenu() {
    elements.modelMenu.replaceChildren();
    const staging = modelMenuStaging();
    const globalKeys = new Set(activeModels().map(modelKey));
    const list = document.createElement("div");
    list.className = "model-menu-list";
    list.setAttribute("role", "group");
    list.setAttribute("aria-label", "可用模型");

    const follow = document.createElement("button");
    follow.type = "button";
    follow.className = "model-menu-item model-menu-follow";
    follow.setAttribute("role", "menuitemcheckbox");
    follow.setAttribute("aria-checked", String(staging.follow));
    follow.classList.toggle("selected", staging.follow);
    const followMark = document.createElement("span");
    followMark.className = "model-mark";
    followMark.textContent = "全";
    const followCopy = document.createElement("span");
    followCopy.className = "model-menu-copy";
    const followName = document.createElement("strong");
    followName.textContent = "跟随全局";
    const followHint = document.createElement("small");
    followHint.textContent = "使用全局激活模型池";
    followCopy.append(followName, followHint);
    const followCheck = document.createElement("span");
    followCheck.className = "icon-slot check-slot";
    followCheck.setAttribute("aria-hidden", "true");
    if (staging.follow) followCheck.appendChild(createIcon("check"));
    follow.append(followMark, followCopy, followCheck);
    follow.addEventListener("click", chooseFollowGlobal);
    list.appendChild(follow);

    for (const model of state.models) {
      if (!model || typeof model !== "object") continue;
      const button = document.createElement("button");
      button.type = "button";
      button.className = "model-menu-item";
      button.setAttribute("role", "menuitemcheckbox");
      button.dataset.modelKey = modelKey(model);
      const checked = staging.follow ? globalKeys.has(button.dataset.modelKey) : staging.keys.has(button.dataset.modelKey);
      const selected = checked && !staging.follow;
      button.setAttribute("aria-checked", String(checked));
      button.classList.toggle("selected", selected);
      button.classList.toggle("from-global", checked && staging.follow);

      const mark = document.createElement("span");
      mark.className = "model-mark";
      mark.textContent = modelMark(model);
      const copy = document.createElement("span");
      copy.className = "model-menu-copy";
      const name = document.createElement("strong");
      name.textContent = String(model.model || "");
      const provider = document.createElement("small");
      provider.textContent = String(model.provider_name || model.provider_id || "");
      copy.append(name, provider);
      const check = document.createElement("span");
      check.className = "icon-slot check-slot";
      check.setAttribute("aria-hidden", "true");
      if (checked) check.appendChild(createIcon("check"));
      button.append(mark, copy, check);
      button.addEventListener("click", () => toggleStagedModel(button.dataset.modelKey));
      list.appendChild(button);
    }

    const footer = document.createElement("footer");
    footer.className = "model-menu-footer";
    footer.setAttribute("role", "none");
    const feedback = document.createElement("span");
    feedback.className = "model-menu-feedback";
    feedback.setAttribute("role", "status");
    feedback.setAttribute("aria-live", "polite");
    const cancel = document.createElement("button");
    cancel.type = "button";
    cancel.className = "model-cancel";
    cancel.setAttribute("role", "menuitem");
    cancel.textContent = "取消";
    cancel.addEventListener("click", () => closeModelMenu({ restoreFocus: true }));
    const confirm = document.createElement("button");
    confirm.type = "button";
    confirm.className = "model-confirm";
    confirm.setAttribute("role", "menuitem");
    confirm.textContent = "确认";
    confirm.addEventListener("click", confirmModelSelection);
    footer.append(feedback, cancel, confirm);
    elements.modelMenu.append(list, footer);
    updateModelMenuState();
    updateCurrentModelDisplay();
    refreshLiveEndpointVisibility();
    updateControlState();
  }

  function updateModelMenuState() {
    const staging = modelMenuStaging();
    const globalKeys = new Set(activeModels().map(modelKey));
    elements.modelMenu.querySelectorAll(".model-menu-item").forEach((button) => {
      const isFollowItem = button.classList.contains("model-menu-follow");
      const key = button.dataset.modelKey || "";
      const checked = isFollowItem
        ? staging.follow
        : (staging.follow ? globalKeys.has(key) : staging.keys.has(key));
      button.classList.toggle("selected", checked && (isFollowItem || !staging.follow));
      button.classList.toggle("from-global", !isFollowItem && checked && staging.follow);
      button.setAttribute("aria-checked", String(checked));
      button.disabled = state.blocked || state.modelSelectionSubmitting;
      const check = button.querySelector(".check-slot");
      if (check) check.replaceChildren(...(checked ? [createIcon("check")] : []));
    });
    const feedback = elements.modelMenu.querySelector(".model-menu-feedback");
    if (feedback) {
      const following = staging.follow || staging.keys.size === 0;
      feedback.textContent = state.modelMenuError
        || (following ? "跟随全局激活模型池" : `已选择 ${formatInteger(staging.keys.size)} 个模型（仅本会话）`);
      feedback.classList.toggle("is-error", Boolean(state.modelMenuError));
    }
    const confirm = elements.modelMenu.querySelector(".model-confirm");
    if (confirm) {
      confirm.textContent = state.modelSelectionSubmitting ? "正在应用" : "确认";
      confirm.disabled = state.modelSelectionSubmitting || state.blocked;
    }
    const cancel = elements.modelMenu.querySelector(".model-cancel");
    if (cancel) cancel.disabled = state.modelSelectionSubmitting;
  }

  function chooseFollowGlobal() {
    if (!(state.stagedModelKeys instanceof Set) || state.modelSelectionSubmitting) return;
    state.stagedFollowGlobal = true;
    state.stagedModelKeys = new Set();
    state.modelMenuTouched = true;
    state.modelMenuError = "";
    updateModelMenuState();
  }

  function toggleStagedModel(key) {
    if (!(state.stagedModelKeys instanceof Set) || state.modelSelectionSubmitting) return;
    if (state.stagedFollowGlobal) {
      // 退出跟随模式：以当前显示的全局激活池为起点继续多选。
      state.stagedFollowGlobal = false;
      state.stagedModelKeys = new Set(activeModels().map(modelKey));
    }
    if (state.stagedModelKeys.has(key)) state.stagedModelKeys.delete(key);
    else state.stagedModelKeys.add(key);
    state.modelMenuTouched = true;
    state.modelMenuError = "";
    updateModelMenuState();
  }

  function newestLiveRun() {
    let latest = null;
    for (const live of state.liveRuns.values()) latest = live;
    return latest;
  }

  function deriveConversationDetails() {
    const live = newestLiveRun();
    if (state.turns.length === 0) {
      const liveUser = live?.userText || state.pendingSubmission?.content || "";
      if (!liveUser) return { title: "新对话", snippet: "尚未开始", timestamp: null };
      return { title: firstLine(liveUser) || "新对话", snippet: firstLine(liveUser), timestamp: new Date() };
    }
    const firstTurn = state.turns[0];
    const lastTurn = state.turns[state.turns.length - 1];
    const followups = Array.isArray(lastTurn?.followups) ? lastTurn.followups : [];
    const lastFollowup = followups[followups.length - 1];
    const assistant = String(lastTurn?.assistant_content || "").trim();
    const liveContent = live ? String(live.userText || "").trim() : "";
    const snippet = firstLine(liveContent || assistant || lastFollowup?.content || lastTurn?.user_content || "");
    const timestamp = liveContent ? live?.startedAt : lastTurn?.assistant_timestamp || lastFollowup?.submitted_at || lastTurn?.user_timestamp;
    return {
      title: firstLine(firstTurn?.user_content) || "当前对话",
      snippet: snippet || (lastTurn?.status === "running" ? "正在回复" : "对话已开始"),
      timestamp
    };
  }

  function multiSessionEnabled() {
    return Boolean(state.capabilities?.multi_conversation);
  }

  function sessionDisplayName(session) {
    const name = firstLine(session?.name || "");
    return name || "新会话";
  }

  function findSession(sessionId) {
    const id = String(sessionId || "");
    return state.sessions.find((session) => String(session?.session_id) === id) || null;
  }


  function viewSessionEntry() {
    return state.viewSessionId ? findSession(state.viewSessionId) : null;
  }

  function trackRun(sessionId, runId) {
    const session = String(sessionId || "");
    const run = String(runId || "");
    if (!session || !run) return;
    let runs = state.runsBySession.get(session);
    if (!runs) {
      runs = new Set();
      state.runsBySession.set(session, runs);
    }
    runs.add(run);
  }

  function untrackRun(runId) {
    const run = String(runId || "");
    for (const [sessionId, runs] of state.runsBySession) {
      if (runs.delete(run) && runs.size === 0) state.runsBySession.delete(sessionId);
    }
  }

  function runSessionId(runId) {
    const run = String(runId || "");
    if (!run) return "";
    for (const [sessionId, runs] of state.runsBySession) {
      if (runs.has(run)) return sessionId;
    }
    return "";
  }

  function sessionHasRuns(sessionId) {
    return (state.runsBySession.get(String(sessionId || ""))?.size || 0) > 0;
  }

  function closeSessionMenu() {
    if (!state.sessionMenuFor) return;
    state.sessionMenuFor = null;
    renderSessionList();
  }

  function toggleSessionMenu(sessionId) {
    state.sessionMenuFor = state.sessionMenuFor === sessionId ? null : sessionId;
    renderSessionList();
    if (!state.sessionMenuFor) return;
    const item = elements.sessionItems.querySelector(`.session-item[data-session-id="${CSS.escape(sessionId)}"]`);
    const menu = item?.querySelector(".session-menu");
    if (menu) {
      const menuRect = menu.getBoundingClientRect();
      const listRect = elements.sessionList.getBoundingClientRect();
      if (menuRect.bottom > listRect.bottom - 4) menu.classList.add("open-up");
      window.requestAnimationFrame(() => menu.querySelector("button")?.focus());
    }
  }

  function beginSessionRename(sessionId) {
    state.sessionRenaming = sessionId;
    renderSessionList();
  }

  function cancelSessionRename() {
    state.sessionRenaming = null;
    renderSessionList();
  }

  async function commitSessionRename(sessionId, value) {
    if (state.sessionRenaming !== sessionId) return;
    state.sessionRenaming = null;
    const session = findSession(sessionId);
    const name = String(value || "").trim();
    if (!session || !name || name === String(session.name || "").trim()) {
      renderSessionList();
      return;
    }
    try {
      await apiRequest(`/api/sessions/${encodeURIComponent(sessionId)}`, {
        method: "PATCH",
        body: JSON.stringify({ name })
      });
      session.name = name;
      showToast("会话已重命名");
    } catch (error) {
      showToast(error.message || "重命名失败", "error");
    }
    renderSessionList();
    if (sessionId === state.viewSessionId) updateConversationChrome();
  }

  function buildSessionMenu(session, isDefault) {
    const id = String(session?.session_id || "");
    const menu = document.createElement("div");
    menu.className = "session-menu";
    menu.setAttribute("role", "menu");
    menu.setAttribute("aria-label", `会话操作：${sessionDisplayName(session)}`);
    // 终端集成会话是固定入口:不可改名、不可删除、不可被顶替,
    // 菜单只留「清空对话」;其余会话不再提供「设为默认」。
    const actions = [];
    if (!isDefault) actions.push({ label: "重命名", handler: () => beginSessionRename(id) });
    if (isDefault) actions.push({ label: "清空对话", handler: requestClearConversation });
    if (!isDefault) actions.push({ label: "删除", danger: true, handler: () => deleteSession(id) });
    for (const action of actions) {
      const button = document.createElement("button");
      button.type = "button";
      button.setAttribute("role", "menuitem");
      if (action.danger) button.classList.add("is-danger");
      button.textContent = action.label;
      button.addEventListener("click", (event) => {
        event.stopPropagation();
        closeSessionMenu();
        action.handler();
      });
      menu.appendChild(button);
    }
    return menu;
  }

  function buildSessionItem(session) {
    const id = String(session?.session_id || "");
    const isView = Boolean(id) && id === state.viewSessionId;
    // 终端集成会话固定为 id "default",不再跟随可变的全局指针。
    const isDefault = id === "default";
    const item = document.createElement("div");
    item.className = `session-item${isView ? " active" : ""}`;
    item.dataset.sessionId = id;

    const renaming = state.sessionRenaming === id;
    const main = document.createElement(renaming ? "div" : "button");
    main.className = `session-item-main${renaming ? " is-renaming" : ""}`;
    if (!renaming) {
      main.type = "button";
      main.title = isView ? sessionDisplayName(session) : `查看「${sessionDisplayName(session)}」`;
      main.addEventListener("click", () => openSessionView(id));
    }
    const icon = id === "default" ? "terminal" : session?.mode === "dev" ? "code" : "message-circle";
    main.appendChild(makeIconSlot(icon));

    const copy = document.createElement("span");
    copy.className = "session-copy";
    if (renaming) {
      const input = document.createElement("input");
      input.className = "session-rename-input";
      input.type = "text";
      input.value = String(session?.name || "");
      input.maxLength = 200;
      input.setAttribute("aria-label", "会话名称");
      input.addEventListener("click", (event) => event.stopPropagation());
      input.addEventListener("keydown", (event) => {
        event.stopPropagation();
        if (event.key === "Enter") {
          event.preventDefault();
          commitSessionRename(id, input.value);
        } else if (event.key === "Escape") {
          event.preventDefault();
          cancelSessionRename();
        }
      });
      input.addEventListener("blur", () => {
        if (state.sessionRenaming === id) commitSessionRename(id, input.value);
      });
      copy.appendChild(input);
      window.requestAnimationFrame(() => {
        input.focus();
        input.select();
      });
    } else {
      const titleRow = document.createElement("span");
      titleRow.className = "session-title-row";
      const title = document.createElement("strong");
      title.textContent = sessionDisplayName(session);
      titleRow.appendChild(title);
      if (isDefault) {
        const badge = document.createElement("span");
        badge.className = "session-default-badge";
        badge.textContent = "默认";
        badge.title = "CLI 与快捷入口的默认会话";
        titleRow.appendChild(badge);
      }
      copy.appendChild(titleRow);
    }

    // Gemini-style list rows: name only; details live in the hover tooltip.
    if (!renaming) {
      const snippet = firstLine(session?.last_user_content || "");
      const workspace = String(session?.workspace || "").trim();
      const details = [snippet, workspace].filter(Boolean).join("\n");
      if (details) {
        main.title = `${sessionDisplayName(session)}\n${details}`;
      }
    }

    main.appendChild(copy);
    item.appendChild(main);

    const trailing = document.createElement("span");
    trailing.className = "session-trailing";

    if (sessionHasRuns(id)) {
      const dot = document.createElement("span");
      dot.className = "session-run-dot";
      dot.title = "有回复正在运行";
      trailing.appendChild(dot);
    }

    const menuButton = document.createElement("button");
    menuButton.type = "button";
    menuButton.className = "session-menu-button";
    menuButton.title = "会话操作";
    menuButton.setAttribute("aria-label", `会话操作：${sessionDisplayName(session)}`);
    menuButton.setAttribute("aria-haspopup", "menu");
    menuButton.setAttribute("aria-expanded", String(state.sessionMenuFor === id));
    menuButton.appendChild(makeIconSlot("ellipsis"));
    menuButton.addEventListener("click", (event) => {
      event.stopPropagation();
      toggleSessionMenu(id);
    });
    trailing.appendChild(menuButton);
    item.appendChild(trailing);

    if (state.sessionMenuFor === id) item.appendChild(buildSessionMenu(session, isDefault));
    return item;
  }

  function buildFallbackSessionItem() {
    const details = deriveConversationDetails();
    const item = document.createElement("div");
    item.className = "session-item active";
    const main = document.createElement("button");
    main.type = "button";
    main.className = "session-item-main";
    main.title = details.title;
    main.appendChild(makeIconSlot("message-circle"));
    const copy = document.createElement("span");
    copy.className = "session-copy";
    const title = document.createElement("strong");
    title.textContent = details.title;
    const snippet = document.createElement("small");
    snippet.className = "session-snippet";
    snippet.textContent = details.snippet;
    snippet.title = details.snippet;
    copy.append(title, snippet);
    main.appendChild(copy);
    main.addEventListener("click", () => {
      closeSidebar();
      scrollToBottom({ force: true, smooth: true });
    });
    item.appendChild(main);
    const trailing = document.createElement("span");
    trailing.className = "session-trailing";
    const time = document.createElement("span");
    time.className = "session-time";
    time.textContent = details.timestamp ? formatRelativeTime(details.timestamp) : "";
    trailing.appendChild(time);
    item.appendChild(trailing);
    return item;
  }

  function renderSessionList() {
    if (!elements.sessionItems) return;
    if (state.sessionRenaming && elements.sessionItems.querySelector(".session-rename-input")) return;
    elements.sessionItems.replaceChildren();
    if (!multiSessionEnabled() || state.sessions.length === 0) {
      elements.sessionItems.appendChild(buildFallbackSessionItem());
      return;
    }
    // 侧栏三段:终端集成会话置顶,其余按会话模式分组(创建时定死)。
    const terminal = state.sessions.filter((session) => String(session?.session_id) === "default");
    const normal = state.sessions.filter(
      (session) => String(session?.session_id) !== "default" && session?.mode !== "dev"
    );
    const dev = state.sessions.filter((session) => session?.mode === "dev");
    for (const session of terminal) elements.sessionItems.appendChild(buildSessionItem(session));
    if (normal.length) {
      elements.sessionItems.appendChild(buildSessionGroupHeader("普通模式"));
      for (const session of normal) elements.sessionItems.appendChild(buildSessionItem(session));
    }
    if (dev.length) {
      elements.sessionItems.appendChild(buildSessionGroupHeader("开发模式"));
      for (const session of dev) elements.sessionItems.appendChild(buildSessionItem(session));
    }
  }

  function buildSessionGroupHeader(label) {
    const header = document.createElement("div");
    header.className = "session-group-header";
    header.textContent = label;
    return header;
  }

  async function refreshSessions() {
    try {
      const response = await apiRequest("/api/sessions");
      const payload = await response.json();
      state.sessions = Array.isArray(payload?.sessions) ? payload.sessions : [];
      renderSessionList();
      updateConversationChrome();
    } catch (_) {
      // 后续 SSE 或 bootstrap 会补齐会话列表。
    }
  }

  function setSessionBusy(value) {
    state.sessionBusy = Boolean(value);
    updateControlState();
  }

  async function createSession(mode) {
    if (state.blocked || state.sessionBusy || state.adminBusy || state.submitting) return;
    setSessionBusy(true);
    try {
      const response = await apiRequest("/api/sessions", {
        method: "POST",
        body: JSON.stringify(mode === "dev" ? { mode: "dev" } : {})
      });
      const payload = await response.json();
      const record = payload?.session && typeof payload.session === "object" ? payload.session : null;
      const sessionId = String(record?.session_id || "");
      if (sessionId && !findSession(sessionId)) {
        state.sessions.unshift(record);
        renderSessionList();
      }
      if (sessionId) await loadSessionView(sessionId);
      focusComposerIfDesktop();
    } catch (error) {
      showToast(error.message || "新建会话失败", "error");
    } finally {
      setSessionBusy(false);
    }
  }

  async function openSessionView(sessionId, { userInitiated = true } = {}) {
    if (!sessionId) return;
    if (sessionId === state.viewSessionId && !state.viewLoading) {
      closeSidebar();
      scrollToBottom({ force: true, smooth: true });
      return;
    }
    await loadSessionView(sessionId, { userInitiated });
  }

  async function loadSessionView(sessionId, { quiet = false, userInitiated = false } = {}) {
    if (!sessionId || (quiet && sessionId !== state.viewSessionId) || (state.viewLoading && !userInitiated)) return;
    const generation = ++state.viewLoadGeneration;
    state.viewLoading = true;
    try {
      const response = await apiRequest(`/api/sessions/${encodeURIComponent(sessionId)}/turns`);
      const payload = await response.json();
      if (generation !== state.viewLoadGeneration) return;
      applySessionView(payload);
      if (!quiet) closeSidebar();
    } catch (error) {
      if (generation !== state.viewLoadGeneration) return;
      if (error.status === 401) showBlockedState(true);
      else if (error.status === 404) {
        showToast("会话不存在", "error");
        refreshSessions();
        if (sessionId === state.viewSessionId) window.setTimeout(() => openFallbackSessionView(sessionId), 0);
      } else showToast(error.message || "载入会话失败", "error");
    } finally {
      if (generation === state.viewLoadGeneration) {
        state.viewLoading = false;
        updateControlState();
      }
    }
  }

  function disposeAllLiveRuns() {
    for (const live of state.liveRuns.values()) disposeLiveState(live);
    state.liveRuns.clear();
    elements.liveStopRail.replaceChildren();
    elements.liveStopRail.hidden = true;
  }

  function applySessionView(payload) {
    const sessionId = String(payload?.session_id || "");
    if (!sessionId) return;
    if (state.viewSessionId && state.viewSessionId !== sessionId && state.composerAttachments.length) {
      clearComposerAttachments(true);
    }
    disposeAllLiveRuns();
    clearViewSyncTimer();
    state.viewSessionId = sessionId;
    if (state.sessionModelOverrideFor !== sessionId) {
      // 会话切换：先按"跟随全局"显示，再异步取回该会话的覆盖池。
      state.sessionModelOverride = null;
      state.sessionModelOverrideFor = "";
      updateCurrentModelDisplay();
      refreshSessionModelOverride(sessionId);
    }
    state.turns = Array.isArray(payload?.turns)
      ? payload.turns.sort((a, b) => asFiniteNumber(a?.seq) - asFiniteNumber(b?.seq))
      : [];
    state.queuedPrompts = Array.isArray(payload?.queued_prompts) ? payload.queued_prompts : [];
    state.redoCandidate = payload?.redo_candidate && typeof payload.redo_candidate === "object"
      ? payload.redo_candidate
      : null;
    closeRevisionEditor();
    state.pendingSubmission = null;
    const runs = (Array.isArray(payload?.runs) ? payload.runs : []).filter((run) => run?.run_id);
    if (runs.length) state.runsBySession.set(sessionId, new Set(runs.map((run) => String(run.run_id))));
    else state.runsBySession.delete(sessionId);
    state.viewRunningTurnId = !runs.length && typeof payload?.running_turn_id === "string" && payload.running_turn_id
      ? payload.running_turn_id
      : null;
    renderConversation();
    renderQueueTray();
    renderJobsStrip();
    restoreLiveRuns(runs);
    updateConversationChrome();
    updateControlState();
    scheduleViewSync();
  }

  function findUnclaimedRunningTurn() {
    const claimed = new Set();
    for (const live of state.liveRuns.values()) {
      if (live.turnId) claimed.add(String(live.turnId));
    }
    return state.turns.find((turn) => turn?.status === "running" && !claimed.has(String(turn?.id))) || null;
  }

  function createLiveForRun(runId, userText = "", options = {}) {
    const { claimTurn = true, operation = "create", turnId = null, inputId = null } = options;
    const existing = state.liveRuns.get(runId);
    if (existing) return existing;
    const redo = operation === "redo";
    const runningTurn = redo || userText || !claimTurn ? null : findUnclaimedRunningTurn();
    const live = createLiveState(runId, {
      turnId: turnId || runningTurn?.id || null,
      userText: userText || runningTurn?.user_content || "",
      userAttachments: runningTurn?.attachments || [],
      startedAt: runningTurn?.user_timestamp || new Date(),
      userRendered: redo || Boolean(runningTurn),
      operation,
      inputId,
      editedContent: options.editedContent
    });
    state.liveRuns.set(runId, live);
    return live;
  }

  function beginRunReplay() {
    // 事件环形缓冲已滚过上限时,after=0 必然触发 resync_required →
    // bootstrap → 又 replay 的循环:短窗口内连续吃到 resync 就放弃从头
    // 重放,live 状态由 bootstrap 快照兜底,增量从当前事件 id 继续。
    const now = Date.now();
    if (state.replayResyncCount >= 2 && now - state.replayResyncAt < 15000) {
      state.replayRunIds = null;
      connectEventSource(state.lastEventId);
      return;
    }
    state.replayRunIds = new Set(state.liveRuns.keys());
    state.replayCutoff = Math.max(state.lastEventId, state.replayCutoff, state.latestEventId);
    state.lastEventId = 0;
    connectEventSource(0);
  }

  function restoreLiveRuns(runs) {
    let restored = false;
    for (const run of runs) {
      const runId = String(run?.run_id || "");
      if (!runId || state.terminalRunIds.has(runId)) continue;
      const live = createLiveForRun(runId, "", {
        operation: String(run?.operation || "create"),
        turnId: String(run?.turn_id || "") || null,
        inputId: String(run?.input_id || "") || null
      });
      if (live.operation === "redo" && state.turns.some((turn) => {
        return String(turn?.id) === String(live.turnId) && turn?.status === "running";
      })) {
        live.redoCommitted = true;
      }
      restored = true;
    }
    if (restored) beginRunReplay();
  }

  async function openFallbackSessionView(excludedSessionId) {
    const excluded = String(excludedSessionId || "");
    if (state.viewSessionId !== excluded) return;
    const fallback = state.currentSessionId && state.currentSessionId !== excluded
      ? state.currentSessionId
      : String(state.sessions.find((session) => String(session?.session_id) !== excluded)?.session_id || "");
    if (fallback) await loadSessionView(fallback);
    else await loadBootstrap();
  }

  async function deleteSession(sessionId) {
    const session = findSession(sessionId);
    if (!window.confirm(`删除会话「${sessionDisplayName(session)}」？此操作无法撤销。`)) return;
    if (state.sessionBusy) return;
    setSessionBusy(true);
    try {
      await apiRequest(`/api/sessions/${encodeURIComponent(sessionId)}`, { method: "DELETE" });
      showToast("会话已删除");
      state.sessions = state.sessions.filter((item) => String(item?.session_id) !== String(sessionId));
      renderSessionList();
      if (sessionId === state.viewSessionId) await openFallbackSessionView(sessionId);
    } catch (error) {
      showToast(error.message || "删除失败", "error");
    } finally {
      setSessionBusy(false);
    }
  }

  function handleSessionEvent(name, data) {
    const sessionId = String(data?.session_id || "");
    if (!sessionId) return;
    if (name === "session.created") {
      if (data?.platform) return;
      if (!findSession(sessionId)) {
        state.sessions.unshift({
          session_id: sessionId,
          name: String(data?.name || ""),
          kind: "",
          workspace: "",
          mode: data?.mode === "dev" ? "dev" : "normal",
          created_at: null,
          updated_at: new Date().toISOString(),
          turn_count: 0,
          last_user_content: ""
        });
        renderSessionList();
      }
    } else if (name === "session.renamed") {
      const target = findSession(sessionId);
      if (target) target.name = String(data?.name || "");
      renderSessionList();
      if (sessionId === state.viewSessionId) updateConversationChrome();
    } else if (name === "session.deleted") {
      state.sessions = state.sessions.filter((item) => String(item?.session_id) !== sessionId);
      renderSessionList();
      if (sessionId === state.viewSessionId && !state.bootstrapPromise && !state.viewLoading) {
        openFallbackSessionView(sessionId);
      }
    } else if (name === "session.updated") {
      const target = findSession(sessionId);
      if (target && Object.prototype.hasOwnProperty.call(data || {}, "workspace")) {
        target.workspace = String(data?.workspace || "");
      }
      if (Object.prototype.hasOwnProperty.call(data || {}, "model_override") && sessionId === state.viewSessionId) {
        setSessionModelOverride(sessionId, data.model_override);
      }
      renderSessionList();
      if (sessionId === state.viewSessionId) updateConversationChrome();
    } else if (name === "session.current_changed") {
      // 每视图独立浏览：默认会话只影响侧栏「默认」徽标，不再跟随切换。
      state.currentSessionId = sessionId;
      renderSessionList();
    }
  }

  function updateConversationChrome() {
    updateModeBadge();
    const details = deriveConversationDetails();
    const current = multiSessionEnabled() ? viewSessionEntry() : null;
    const title = current ? sessionDisplayName(current) : details.title;
    elements.conversationTitle.textContent = title;
    elements.conversationTitle.title = title;
    const workspace = String(current?.workspace || "").trim();
    let meta;
    if (conversationRunning()) {
      meta = state.liveRuns.size > 1 ? `${formatInteger(state.liveRuns.size)} 路回复进行中` : "正在回复";
    } else meta = details.timestamp ? formatRelativeTime(details.timestamp) : "尚未开始";
    elements.conversationMeta.textContent = workspace ? `${meta} · ${workspace}` : meta;
    elements.conversationMeta.title = workspace;
    renderSessionList();
  }

  function conversationRunning() {
    return state.liveRuns.size > 0 || Boolean(state.viewRunningTurnId);
  }

  function activeTurnUpdateTarget(sessionId) {
    const runIds = state.runsBySession.get(String(sessionId || ""));
    if (!runIds) return null;
    const candidates = [...runIds]
      .map((runId) => state.liveRuns.get(String(runId)))
      .filter((live) => live && !live.ended && live.turnId);
    if (candidates.length !== 1) return null;
    return { runId: candidates[0].runId, turnId: candidates[0].turnId };
  }

  function hasPendingQuestion() {
    for (const live of state.liveRuns.values()) {
      for (const question of live.questions.values()) {
        if (question.pending) return true;
      }
    }
    return false;
  }

  function countCharacters(value) {
    return Array.from(String(value || "")).length;
  }

  // 触屏设备上程序化聚焦会弹出软键盘挡住内容，只在桌面端自动聚焦
  function focusComposerIfDesktop() {
    if (window.matchMedia("(hover: none), (pointer: coarse)").matches) return;
    elements.composerInput.focus();
  }

  function resizeComposer() {
    const input = elements.composerInput;
    input.style.height = "auto";
    input.style.height = `${Math.min(input.scrollHeight, layoutViewportWidth() <= 760 ? 120 : 146)}px`;
    const count = countCharacters(input.value);
    elements.characterCount.textContent = `${formatInteger(count)} / 20,000`;
    elements.characterCount.hidden = count < 18_000;
    elements.characterCount.classList.toggle("is-error", count > MAX_CONTENT_CHARS);
    updateControlState();
    window.requestAnimationFrame(updateJumpButtonOffset);
  }

  function formatFileSize(value) {
    const bytes = Math.max(0, asFiniteNumber(value));
    if (bytes < 1024) return `${Math.round(bytes)} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(bytes < 10 * 1024 ? 1 : 0)} KB`;
    return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  }

  function safeAttachmentUrl(value) {
    const raw = String(value || "").trim();
    if (!raw) return null;
    try {
      const url = new URL(raw, window.location.origin);
      if (url.origin !== window.location.origin || !url.pathname.startsWith("/api/attachments/") || url.pathname === "/api/attachments/") return null;
      return url.href;
    } catch (_) {
      return null;
    }
  }

  function attachmentSessionId() {
    return String(state.viewSessionId || state.currentSessionId || "");
  }

  function renderComposerAttachments() {
    const tray = elements.attachmentTray;
    tray.replaceChildren();
    tray.hidden = state.composerAttachments.length === 0;
    for (const item of state.composerAttachments) {
      const isImage = item.kind === "image" && item.previewUrl;
      const entry = document.createElement("div");
      entry.className = `attachment-item ${isImage ? "is-image" : "is-file"} is-${item.status}`;
      entry.title = item.status === "error" ? `${item.name}: ${item.error || "上传失败"}` : item.name;
      if (isImage) {
        const image = document.createElement("img");
        image.src = item.previewUrl;
        image.alt = "";
        const fallback = document.createElement("span");
        fallback.className = "attachment-image-fallback";
        fallback.hidden = true;
        fallback.appendChild(makeIconSlot("circle-alert"));
        image.addEventListener("load", () => { fallback.hidden = true; }, { once: true });
        image.addEventListener("error", () => {
          image.hidden = true;
          fallback.hidden = false;
        }, { once: true });
        entry.append(image, fallback);
      } else {
        const icon = document.createElement("span");
        icon.className = "attachment-file-icon";
        const nameParts = String(item.name || "").split(".");
        const extension = nameParts.length > 1 ? nameParts.pop().toUpperCase() : "FILE";
        icon.textContent = extension.slice(0, 4);
        entry.appendChild(icon);
        const copy = document.createElement("span");
        copy.className = "attachment-item-copy";
        const name = document.createElement("strong");
        name.textContent = item.name;
        name.title = item.name;
        const meta = document.createElement("small");
        if (item.status === "uploading") meta.textContent = `上传中 ${Math.round(item.progress || 0)}%`;
        else if (item.status === "error") meta.textContent = item.error || "上传失败";
        else meta.textContent = formatFileSize(item.size);
        copy.append(name, meta);
        entry.appendChild(copy);
      }
      if (item.status === "uploading") {
        const spinner = makeIconSlot("loader-circle", "attachment-spinner is-spinning");
        entry.appendChild(spinner);
      } else if (item.status === "error") {
        const retry = document.createElement("button");
        retry.type = "button";
        retry.className = "attachment-action";
        retry.title = "重试上传";
        retry.setAttribute("aria-label", `重试上传 ${item.name}`);
        retry.appendChild(makeIconSlot("refresh-cw"));
        retry.addEventListener("click", () => uploadComposerAttachment(item));
        entry.appendChild(retry);
      }
      const remove = document.createElement("button");
      remove.type = "button";
      remove.className = "attachment-action attachment-remove";
      remove.title = "移除附件";
      remove.setAttribute("aria-label", `移除附件 ${item.name}`);
      remove.appendChild(makeIconSlot("x"));
      remove.addEventListener("click", () => removeComposerAttachment(item));
      entry.appendChild(remove);
      tray.appendChild(entry);
    }
    window.requestAnimationFrame(updateJumpButtonOffset);
  }

  function uploadComposerAttachment(item) {
    if (!item?.file || !item.sessionId) return;
    item.status = "uploading";
    item.progress = 0;
    item.error = "";
    renderComposerAttachments();
    updateControlState();
    const request = new XMLHttpRequest();
    item.request = request;
    request.open("POST", `/api/attachments?session_id=${encodeURIComponent(item.sessionId)}`);
    request.setRequestHeader("Accept", "application/json");
    request.setRequestHeader("Content-Type", item.file.type || "application/octet-stream");
    request.setRequestHeader("X-Miyu-Filename", encodeURIComponent(item.file.name));
    request.upload.addEventListener("progress", (event) => {
      if (!event.lengthComputable || item.request !== request) return;
      item.progress = Math.min(99, Math.round((event.loaded / event.total) * 100));
      renderComposerAttachments();
    });
    request.addEventListener("load", () => {
      if (item.request !== request) return;
      item.request = null;
      let payload = null;
      try { payload = JSON.parse(request.responseText || "null"); } catch (_) {}
      if (request.status >= 200 && request.status < 300 && payload?.id) {
        const uploadedPreview = payload.kind === "image" ? safeAttachmentUrl(payload.url) : null;
        if (uploadedPreview && item.previewUrl?.startsWith("blob:")) URL.revokeObjectURL(item.previewUrl);
        Object.assign(item, payload, {
          previewUrl: uploadedPreview || item.previewUrl,
          status: "ready",
          progress: 100,
          error: ""
        });
      } else {
        item.status = "error";
        item.error = payload?.error?.message || `上传失败 (${request.status || "网络错误"})`;
      }
      renderComposerAttachments();
      updateControlState();
    });
    request.addEventListener("error", () => {
      if (item.request !== request) return;
      item.request = null;
      item.status = "error";
      item.error = "无法连接上传服务";
      renderComposerAttachments();
      updateControlState();
    });
    request.send(item.file);
  }

  function collectTransferFiles(transfer) {
    const files = [];
    const seen = new Set();
    const add = (file) => {
      if (!(file instanceof File)) return;
      const key = `${file.name}\0${file.size}\0${file.lastModified}\0${file.type}`;
      if (seen.has(key)) return;
      seen.add(key);
      files.push(file);
    };
    for (const item of Array.from(transfer?.items || [])) {
      if (item.kind === "file") add(item.getAsFile());
    }
    for (const file of Array.from(transfer?.files || [])) add(file);
    return files;
  }

  function addComposerFiles(files) {
    if (!state.capabilities?.attachments) return;
    const incoming = Array.isArray(files) ? files : Array.from(files || []);
    if (!incoming.length) return;
    const available = Math.max(0, MAX_ATTACHMENTS - state.composerAttachments.length);
    if (incoming.length > available) {
      showToast(`每条消息最多添加 ${MAX_ATTACHMENTS} 个附件，已忽略 ${incoming.length - available} 个`, "error");
    }
    const accepted = incoming.slice(0, available);
    const existingBytes = state.composerAttachments.reduce((sum, item) => sum + asFiniteNumber(item.size), 0);
    let totalBytes = existingBytes;
    for (const file of accepted) {
      if (!(file instanceof File) || file.size <= 0 || file.size > MAX_ATTACHMENT_BYTES) {
        showToast(`${file?.name || "附件"} 必须小于 10 MB`, "error");
        continue;
      }
      if (totalBytes + file.size > MAX_ATTACHMENT_TOTAL_BYTES) {
        showToast("单条消息的附件总计不能超过 32 MB", "error");
        break;
      }
      totalBytes += file.size;
      const image = file.type.startsWith("image/");
      const item = {
        localId: `${Date.now()}-${Math.random().toString(16).slice(2)}`,
        file,
        sessionId: attachmentSessionId(),
        name: file.name,
        mime: file.type,
        kind: image ? "image" : "text",
        size: file.size,
        status: "uploading",
        progress: 0,
        previewUrl: image ? URL.createObjectURL(file) : "",
        request: null,
        error: ""
      };
      state.composerAttachments.push(item);
      uploadComposerAttachment(item);
    }
    renderComposerAttachments();
    updateControlState();
  }

  function removeComposerAttachment(item, deleteRemote = true) {
    item.request?.abort();
    item.request = null;
    state.composerAttachments = state.composerAttachments.filter((candidate) => candidate !== item);
    if (item.previewUrl) URL.revokeObjectURL(item.previewUrl);
    if (deleteRemote && item.id && item.sessionId) {
      apiRequest(`/api/attachments/${encodeURIComponent(item.id)}?session_id=${encodeURIComponent(item.sessionId)}`, { method: "DELETE" }).catch(() => {});
    }
    renderComposerAttachments();
    updateControlState();
  }

  function clearComposerAttachments(deleteRemote = true) {
    for (const item of [...state.composerAttachments]) removeComposerAttachment(item, deleteRemote);
    elements.attachmentInput.value = "";
  }

  function committedComposerAttachments() {
    const attachments = state.composerAttachments.filter((item) => item.status === "ready").map((item) => ({
      id: item.id,
      url: item.url,
      name: item.name,
      mime: item.mime,
      kind: item.kind,
      size: item.size,
      width: item.width || 0,
      height: item.height || 0
    }));
    clearComposerAttachments(false);
    return attachments;
  }

  function updateJumpButtonOffset() {
    elements.jumpBottomButton.style.bottom = `${elements.composerDock.offsetHeight + 10}px`;
  }

  function updateControlState() {
    const running = conversationRunning();
    const busy = state.adminBusy || state.submitting;
    const locked = state.blocked || state.adminBusy || state.modeChooserOpen;
    const inputCount = countCharacters(elements.composerInput.value.trim());
    const attachmentUploading = state.composerAttachments.some((item) => item.status === "uploading");
    const attachmentError = state.composerAttachments.some((item) => item.status === "error");
    const attachmentReady = state.composerAttachments.some((item) => item.status === "ready");

    elements.composerInput.disabled = locked;
    elements.composerForm.classList.toggle("is-disabled", locked);
    elements.attachButton.disabled = locked || state.submitting || !state.capabilities?.attachments || state.composerAttachments.length >= MAX_ATTACHMENTS;
    elements.newChatButton.disabled = state.blocked || busy || state.sessionBusy || state.viewLoading;
    // 会话级模型覆盖允许在回复进行中调整，下一轮生效。
    elements.modelButton.disabled = state.blocked || state.models.length === 0;
    elements.thinkingVariantButton.disabled = state.blocked || running || busy
      || state.thinkingVariantLoading || state.thinkingVariantModels.length === 0;
    if (elements.thinkingVariantButton.disabled) closeThinkingVariantPopover();
    updateThinkingVariantTrigger();
    elements.promptGrid.querySelectorAll("button").forEach((button) => {
      button.disabled = state.blocked || running || busy;
    });
    updateModelMenuState();

    elements.sendButton.classList.remove("is-cancel");
    elements.sendButton.querySelector(".icon-slot").replaceChildren(createIcon("arrow-up"));
    elements.sendButton.title = running ? "加入队列" : "发送消息";
    elements.sendButton.setAttribute("aria-label", elements.sendButton.title);
    elements.sendButton.disabled = state.blocked || state.adminBusy || state.submitting || hasPendingQuestion()
      || (inputCount === 0 && !attachmentReady) || inputCount > MAX_CONTENT_CHARS || attachmentUploading || attachmentError;
    document.querySelectorAll(".edit-action, .redo-action").forEach((button) => {
      button.disabled = !revisionEligible();
    });

    if (state.blocked) elements.composerState.textContent = "未授权";
    else if (hasPendingQuestion()) elements.composerState.textContent = "等待回答";
    else if (attachmentUploading) elements.composerState.textContent = "正在上传";
    else if (attachmentError) elements.composerState.textContent = "附件上传失败";
    else if (busy) elements.composerState.textContent = state.submitting ? (running ? "正在加入队列" : "正在发送") : "正在处理";
    else if (inputCount > MAX_CONTENT_CHARS) elements.composerState.textContent = "消息不能超过 20,000 个字符";
    else elements.composerState.textContent = "";
    elements.composerState.classList.toggle("is-error", inputCount > MAX_CONTENT_CHARS || attachmentError);
    updateSettingsControls();
  }

  function isNearBottom() {
    const distance = elements.chatScroll.scrollHeight - elements.chatScroll.scrollTop - elements.chatScroll.clientHeight;
    return distance <= NEAR_BOTTOM_PX;
  }

  function isAtBottom() {
    const distance = elements.chatScroll.scrollHeight - elements.chatScroll.scrollTop - elements.chatScroll.clientHeight;
    return distance <= 2;
  }

  function suspendOutputFollowing() {
    state.followOutput = false;
    state.scrollRequestId += 1;
    elements.jumpBottomButton.hidden = false;
  }

  function scrollToBottom({ force = false, smooth = false } = {}) {
    if (!force && !state.followOutput) {
      elements.jumpBottomButton.hidden = false;
      return;
    }
    if (force) state.followOutput = true;
    const requestId = ++state.scrollRequestId;
    window.requestAnimationFrame(() => {
      if (!force && (!state.followOutput || requestId !== state.scrollRequestId)) return;
      state.programmaticScroll = true;
      elements.chatScroll.scrollTo({ top: elements.chatScroll.scrollHeight, behavior: smooth ? "smooth" : "auto" });
      state.nearBottom = true;
      elements.jumpBottomButton.hidden = true;
      window.setTimeout(() => {
        state.programmaticScroll = false;
      }, smooth ? 300 : 0);
    });
  }

  function contentAdded() {
    if (state.followOutput) scrollToBottom();
    else elements.jumpBottomButton.hidden = false;
  }

  async function copyText(text) {
    const value = String(text || "");
    if (!value) return false;
    try {
      if (navigator.clipboard?.writeText) {
        await navigator.clipboard.writeText(value);
        showToast("已复制");
        return true;
      }
    } catch (_) {
      // Use the selection fallback below.
    }
    const textarea = document.createElement("textarea");
    textarea.value = value;
    textarea.setAttribute("readonly", "");
    textarea.style.position = "fixed";
    textarea.style.left = "-9999px";
    textarea.style.top = "0";
    document.body.appendChild(textarea);
    textarea.select();
    textarea.setSelectionRange(0, textarea.value.length);
    let copied = false;
    try {
      copied = document.execCommand("copy");
    } catch (_) {
      copied = false;
    }
    textarea.remove();
    showToast(copied ? "已复制" : "复制失败", copied ? "info" : "error");
    return copied;
  }

  function makeCopyButton(textProvider, label = "复制") {
    const button = document.createElement("button");
    button.type = "button";
    button.title = label;
    button.setAttribute("aria-label", label);
    button.appendChild(makeIconSlot("copy"));
    button.addEventListener("click", () => copyText(typeof textProvider === "function" ? textProvider() : textProvider));
    return button;
  }

  function makeMessageAction(icon, label, handler) {
    const button = document.createElement("button");
    button.type = "button";
    button.title = label;
    button.setAttribute("aria-label", label);
    button.appendChild(makeIconSlot(icon));
    button.addEventListener("click", handler);
    return button;
  }

  function revisionEligible(candidate = state.redoCandidate) {
    if (!candidate || !state.capabilities?.redo) return false;
    return !state.blocked && !state.viewLoading && !state.resyncing
      && !conversationRunning() && !state.submitting && !state.revisionSubmitting
      && !state.adminBusy && !state.sessionBusy && !hasPendingQuestion()
      && state.queuedPrompts.length === 0;
  }

  function closeRevisionEditor({ restoreFocus = false } = {}) {
    const editor = state.revisionEditor;
    if (!editor) return;
    editor.form.remove();
    editor.bubble.hidden = editor.wasHidden;
    state.revisionEditor = null;
    if (restoreFocus) editor.opener?.focus();
  }

  function openRevisionEditor(article, bubble, content, candidate, opener) {
    if (!revisionEligible(candidate)) return;
    closeRevisionEditor();
    const form = document.createElement("form");
    form.className = "revision-editor";
    form.setAttribute("aria-label", "编辑最后一条消息");
    const textarea = document.createElement("textarea");
    textarea.value = String(content || "");
    textarea.maxLength = MAX_CONTENT_CHARS;
    textarea.setAttribute("aria-label", "消息内容");
    const error = document.createElement("div");
    error.className = "revision-editor-error";
    error.setAttribute("role", "alert");
    error.hidden = true;
    const footer = document.createElement("div");
    footer.className = "revision-editor-footer";
    const cancel = document.createElement("button");
    cancel.type = "button";
    cancel.textContent = "取消";
    const submit = document.createElement("button");
    submit.type = "submit";
    submit.textContent = "发送";
    footer.append(cancel, submit);
    form.append(textarea, error, footer);
    const wasHidden = bubble.hidden;
    bubble.hidden = true;
    article.insertBefore(form, article.querySelector(".message-actions"));
    state.revisionEditor = { form, textarea, error, submit, bubble, wasHidden, opener, candidate };
    cancel.addEventListener("click", () => closeRevisionEditor({ restoreFocus: true }));
    form.addEventListener("submit", async (event) => {
      event.preventDefault();
      const draft = textarea.value.trim();
      if (!draft && !article.querySelector(".user-attachments")) {
        error.textContent = "消息不能为空";
        error.hidden = false;
        return;
      }
      if (countCharacters(draft) > MAX_CONTENT_CHARS) {
        error.textContent = "消息不能超过 20,000 个字符";
        error.hidden = false;
        return;
      }
      await submitRedo(candidate, draft);
    });
    textarea.addEventListener("keydown", (event) => {
      if (event.key === "Escape") {
        event.preventDefault();
        closeRevisionEditor({ restoreFocus: true });
      } else if ((event.ctrlKey || event.metaKey) && event.key === "Enter" && !event.isComposing) {
        event.preventDefault();
        form.requestSubmit();
      }
    });
    window.requestAnimationFrame(() => {
      textarea.focus();
      textarea.setSelectionRange(textarea.value.length, textarea.value.length);
      form.scrollIntoView({ block: "nearest" });
    });
  }

  async function submitRedo(candidate, editedContent = null) {
    if (!revisionEligible(candidate)) return;
    const sessionId = state.viewSessionId;
    if (!sessionId) return;
    state.revisionSubmitting = true;
    const editor = state.revisionEditor;
    if (editor) {
      editor.form.setAttribute("aria-busy", "true");
      editor.textarea.disabled = true;
      editor.submit.disabled = true;
      editor.error.hidden = true;
    }
    updateControlState();
    try {
      const body = {
        expected_revision: candidate.revision,
        input_id: candidate.input_id
      };
      if (editedContent != null) body.content = editedContent;
      const response = await apiRequest(
        `/api/sessions/${encodeURIComponent(sessionId)}/turns/${encodeURIComponent(candidate.turn_id)}/redo`,
        { method: "POST", body: JSON.stringify(body) }
      );
      const payload = await response.json();
      const runId = String(payload?.run_id || "");
      if (!runId) throw new ApiError("服务未返回运行标识", response.status);
      trackRun(sessionId, runId);
      createLiveForRun(runId, "", {
        claimTurn: false,
        operation: "redo",
        turnId: candidate.turn_id,
        inputId: candidate.input_id,
        editedContent
      });
      state.redoCandidate = null;
      renderSessionList();
      updateConversationChrome();
    } catch (error) {
      if (editor && state.revisionEditor === editor) {
        editor.error.textContent = error.status === 409 ? "会话已变化，请重新操作" : error.message;
        editor.error.hidden = false;
      }
      showToast(error.status === 409 ? "会话状态已更新" : error.message, "error");
      if (error.status === 409) await loadSessionView(sessionId, { quiet: true });
    } finally {
      state.revisionSubmitting = false;
      if (editor && state.revisionEditor === editor) {
        editor.form.removeAttribute("aria-busy");
        editor.textarea.disabled = false;
        editor.submit.disabled = false;
      }
      updateControlState();
    }
  }

  function validHttpUrl(value) {
    const raw = String(value || "").trim();
    if (!/^https?:\/\//i.test(raw)) return null;
    try {
      const url = new URL(raw);
      return url.protocol === "http:" || url.protocol === "https:" ? url.href : null;
    } catch (_) {
      return null;
    }
  }

  function appendInline(parent, source, depth = 0) {
    const text = String(source || "");
    if (depth > 8) {
      parent.appendChild(document.createTextNode(text));
      return;
    }
    let index = 0;
    let plainStart = 0;
    const flushPlain = (end) => {
      if (end > plainStart) parent.appendChild(document.createTextNode(text.slice(plainStart, end)));
    };
    while (index < text.length) {
      if (text[index] === "\\" && text[index + 1] === "(") {
        const end = text.indexOf("\\)", index + 2);
        if (end > index + 1) {
          flushPlain(index);
          renderMathInto(parent, text.slice(index + 2, end), false);
          index = end + 2;
          plainStart = index;
          continue;
        }
      }
      if (text[index] === "\\" && index + 1 < text.length && "\\`*_[]|~$".includes(text[index + 1])) {
        flushPlain(index);
        parent.appendChild(document.createTextNode(text[index + 1]));
        index += 2;
        plainStart = index;
        continue;
      }
      if (text[index] === "$") {
        if (text[index + 1] === "$") {
          const end = text.indexOf("$$", index + 2);
          if (end > index + 1) {
            flushPlain(index);
            renderMathInto(parent, text.slice(index + 2, end), false);
            index = end + 2;
            plainStart = index;
            continue;
          }
        } else {
          // 行内 $…$:内容非空、不跨行、两端非空格,右 $ 后不紧跟数字(避开价格写法)。
          const end = text.indexOf("$", index + 1);
          const inner = end > index ? text.slice(index + 1, end) : "";
          if (
            end > index + 1
            && inner.length
            && !inner.includes("\n")
            && !/^\s/.test(inner)
            && !/\s$/.test(inner)
            && !/^\d/.test(text.slice(end + 1))
          ) {
            flushPlain(index);
            renderMathInto(parent, inner, false);
            index = end + 1;
            plainStart = index;
            continue;
          }
        }
      }
      if (text[index] === "\n") {
        flushPlain(index);
        parent.appendChild(document.createElement("br"));
        index += 1;
        plainStart = index;
        continue;
      }
      if (text[index] === "`") {
        const end = text.indexOf("`", index + 1);
        if (end > index + 1) {
          flushPlain(index);
          const code = document.createElement("code");
          code.textContent = text.slice(index + 1, end);
          parent.appendChild(code);
          index = end + 1;
          plainStart = index;
          continue;
        }
      }
      if (text[index] === "[") {
        const labelEnd = text.indexOf("](", index + 1);
        const urlEnd = labelEnd >= 0 ? text.indexOf(")", labelEnd + 2) : -1;
        if (labelEnd > index + 1 && urlEnd > labelEnd + 2) {
          const href = validHttpUrl(text.slice(labelEnd + 2, urlEnd));
          if (href) {
            flushPlain(index);
            const link = document.createElement("a");
            link.href = href;
            link.target = "_blank";
            link.rel = "noopener noreferrer";
            appendInline(link, text.slice(index + 1, labelEnd), depth + 1);
            parent.appendChild(link);
            index = urlEnd + 1;
            plainStart = index;
            continue;
          }
        }
      }
      if (text.startsWith("~~", index)) {
        const end = text.indexOf("~~", index + 2);
        if (end > index + 2 && text.slice(index + 2, end).trim()) {
          flushPlain(index);
          const deletion = document.createElement("del");
          appendInline(deletion, text.slice(index + 2, end), depth + 1);
          parent.appendChild(deletion);
          index = end + 2;
          plainStart = index;
          continue;
        }
      }
      const strongMarker = text.startsWith("**", index) ? "**" : text.startsWith("__", index) ? "__" : null;
      if (strongMarker) {
        const end = text.indexOf(strongMarker, index + 2);
        if (end > index + 2 && text.slice(index + 2, end).trim()) {
          flushPlain(index);
          const strong = document.createElement("strong");
          appendInline(strong, text.slice(index + 2, end), depth + 1);
          parent.appendChild(strong);
          index = end + 2;
          plainStart = index;
          continue;
        }
      }
      if (text[index] === "*" || text[index] === "_") {
        const marker = text[index];
        const end = text.indexOf(marker, index + 1);
        if (end > index + 1 && text.slice(index + 1, end).trim()) {
          flushPlain(index);
          const emphasis = document.createElement("em");
          appendInline(emphasis, text.slice(index + 1, end), depth + 1);
          parent.appendChild(emphasis);
          index = end + 1;
          plainStart = index;
          continue;
        }
      }
      index += 1;
    }
    flushPlain(text.length);
  }

  function codeBlock(language, codeText) {
    const wrapper = document.createElement("div");
    wrapper.className = "code-block";
    const toolbar = document.createElement("div");
    toolbar.className = "code-toolbar";
    const label = document.createElement("span");
    label.textContent = language || "代码";
    const copy = makeCopyButton(codeText, "复制代码");
    copy.className = "code-copy-button";
    toolbar.append(label, copy);
    const pre = document.createElement("pre");
    const code = document.createElement("code");
    if (language) code.className = `language-${language}`;
    code.textContent = codeText;
    pre.appendChild(code);
    wrapper.append(toolbar, pre);
    return wrapper;
  }

  function parseTableRow(line) {
    const text = String(line || "").trim();
    const cells = [];
    let cell = "";
    let codeFenceLength = 0;
    let hasSeparator = false;
    let endedWithSeparator = false;
    for (let index = 0; index < text.length;) {
      if (text[index] === "\\" && index + 1 < text.length) {
        cell += text.slice(index, index + 2);
        index += 2;
        endedWithSeparator = false;
        continue;
      }
      if (text[index] === "`") {
        let end = index + 1;
        while (end < text.length && text[end] === "`") end += 1;
        const runLength = end - index;
        if (!codeFenceLength) codeFenceLength = runLength;
        else if (codeFenceLength === runLength) codeFenceLength = 0;
        cell += text.slice(index, end);
        index = end;
        endedWithSeparator = false;
        continue;
      }
      if (text[index] === "|" && !codeFenceLength) {
        cells.push(cell.trim());
        cell = "";
        hasSeparator = true;
        endedWithSeparator = true;
        index += 1;
        continue;
      }
      cell += text[index];
      endedWithSeparator = false;
      index += 1;
    }
    cells.push(cell.trim());
    if (text.startsWith("|")) cells.shift();
    if (endedWithSeparator) cells.pop();
    return { cells, hasSeparator };
  }

  function tableAlignments(line) {
    const row = parseTableRow(line);
    if (!row.hasSeparator || !row.cells.length) return null;
    const alignments = [];
    for (const cell of row.cells) {
      const marker = cell.match(/^(:)?-{3,}(:)?$/);
      if (!marker) return null;
      alignments.push(marker[1] && marker[2] ? "center" : marker[2] ? "right" : marker[1] ? "left" : "");
    }
    return alignments;
  }

  function isTableStart(lines, index) {
    if (index + 1 >= lines.length) return false;
    const header = parseTableRow(lines[index]);
    const alignments = tableAlignments(lines[index + 1]);
    return Boolean(alignments && header.hasSeparator && header.cells.length === alignments.length);
  }

  function isHorizontalRule(line) {
    const text = String(line || "").trim();
    return /^(?:\*\s*){3,}$/.test(text) || /^(?:-\s*){3,}$/.test(text) || /^(?:_\s*){3,}$/.test(text);
  }

  function markdownTable(lines, startIndex) {
    const headers = parseTableRow(lines[startIndex]).cells;
    const alignments = tableAlignments(lines[startIndex + 1]);
    const wrapper = document.createElement("div");
    wrapper.className = "markdown-table-scroll";
    const table = document.createElement("table");
    const head = document.createElement("thead");
    const headRow = document.createElement("tr");
    headers.forEach((content, column) => {
      const cell = document.createElement("th");
      cell.scope = "col";
      if (alignments[column]) cell.className = `align-${alignments[column]}`;
      appendInline(cell, content);
      headRow.appendChild(cell);
    });
    head.appendChild(headRow);
    table.appendChild(head);

    const body = document.createElement("tbody");
    let index = startIndex + 2;
    while (index < lines.length && lines[index].trim()) {
      const row = parseTableRow(lines[index]);
      if (!row.hasSeparator) break;
      const tableRow = document.createElement("tr");
      for (let column = 0; column < headers.length; column += 1) {
        const cell = document.createElement("td");
        if (alignments[column]) cell.className = `align-${alignments[column]}`;
        appendInline(cell, row.cells[column] || "");
        tableRow.appendChild(cell);
      }
      body.appendChild(tableRow);
      index += 1;
    }
    if (body.children.length) table.appendChild(body);
    wrapper.appendChild(table);
    return { node: wrapper, nextIndex: index };
  }

  function isMarkdownBlockStart(lines, index) {
    const line = lines[index];
    return /^\s*```/.test(line) || /^#{1,6}\s+/.test(line) || /^\s*[-*+]\s+/.test(line) || /^\s*\d+[.)]\s+/.test(line) || /^\s*>/.test(line) || isHorizontalRule(line) || isTableStart(lines, index) || /^\s*\$\$/.test(line) || /^\s*\\\[\s*$/.test(line) || Boolean(videoSourceFor(line));
  }

  /* ── 视频消息:整行只有一个视频 URL / 本地路径(或指向它的 markdown 链接)
     时升级为播放器。本地文件经 /api/media 流式端点(带 HTTP Range)。 ── */
  const VIDEO_SOURCE_PATTERN = /\.(mp4|m4v|webm|mov|mkv|ogv)(\?[^\s)]*)?$/i;
  function videoSourceFor(rawLine) {
    const trimmed = String(rawLine || "").trim();
    if (!trimmed || trimmed.length > 2048) return null;
    const link = trimmed.match(/^\[([^\]]*)\]\(([^)\s]+)\)$/);
    const target = link ? link[2] : trimmed;
    if (/\s/.test(target) || !VIDEO_SOURCE_PATTERN.test(target)) return null;
    if (/^https?:\/\//i.test(target)) {
      return { src: target, label: link?.[1] || target };
    }
    if (target.startsWith("/") || target.startsWith("~/")) {
      return {
        src: `/api/media?path=${encodeURIComponent(target)}`,
        label: link?.[1] || target.split("/").pop() || target,
      };
    }
    return null;
  }

  function videoNode(source) {
    const card = document.createElement("div");
    card.className = "video-card";
    const shell = document.createElement("div");
    shell.className = "video-shell";
    const video = document.createElement("video");
    video.controls = true;
    video.preload = "metadata";
    video.playsInline = true;
    video.src = source.src;
    const button = document.createElement("button");
    button.type = "button";
    button.className = "vfs-btn";
    button.title = "网页全屏";
    button.setAttribute("aria-label", "网页全屏");
    button.innerHTML =
      '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M8 3H5a2 2 0 0 0-2 2v3m18 0V5a2 2 0 0 0-2-2h-3m0 18h3a2 2 0 0 0 2-2v-3M3 16v3a2 2 0 0 0 2 2h3"/></svg>';
    button.addEventListener("click", () => shell.classList.toggle("webfs"));
    shell.append(video, button);
    const caption = document.createElement("div");
    caption.className = "video-caption";
    caption.textContent = source.label;
    card.append(shell, caption);
    return card;
  }

  /* ── LaTeX 公式(KaTeX):块级 $$…$$ / \[…\],行内 $…$ / \(…\)。
     katex 未就绪或语法错误时原样降级;流式期间未闭合的定界符保持原文,
     闭合后的下一次重渲染自动升级成公式。 */
  function renderMathInto(parent, tex, displayMode) {
    const trimmed = tex.trim();
    if (trimmed && window.katex && typeof window.katex.render === "function") {
      const node = document.createElement(displayMode ? "div" : "span");
      node.className = displayMode ? "math-display" : "math-inline";
      try {
        window.katex.render(trimmed, node, { displayMode, throwOnError: false, strict: "ignore" });
        parent.appendChild(node);
        return;
      } catch (_) { /* 落到原样文本 */ }
    }
    parent.appendChild(document.createTextNode(displayMode ? `$$${tex}$$` : `$${tex}$`));
  }

  function matchMathBlock(lines, index) {
    const trimmed = lines[index].trim();
    for (const [open, close] of [["$$", "$$"], ["\\[", "\\]"]]) {
      if (!trimmed.startsWith(open)) continue;
      const rest = trimmed.slice(open.length);
      if (rest.length > close.length && rest.endsWith(close)) {
        return { tex: rest.slice(0, rest.length - close.length), nextIndex: index + 1 };
      }
      const body = rest && rest !== close ? [rest] : [];
      let cursor = index + 1;
      while (cursor < lines.length) {
        const candidate = lines[cursor].trim();
        if (candidate === close || candidate.endsWith(close)) {
          if (candidate !== close) body.push(candidate.slice(0, candidate.length - close.length));
          return { tex: body.join("\n"), nextIndex: cursor + 1 };
        }
        body.push(lines[cursor]);
        cursor += 1;
      }
      return null; // 未闭合:保持原文(流式中)
    }
    return null;
  }

  function renderMarkdown(container, source) {
    const lines = String(source || "").replace(/\r\n?/g, "\n").split("\n");
    const fragment = document.createDocumentFragment();
    let index = 0;
    while (index < lines.length) {
      const line = lines[index];
      if (!line.trim()) {
        index += 1;
        continue;
      }
      const fence = line.match(/^\s*```\s*([\w.+-]*)\s*$/);
      if (fence) {
        const codeLines = [];
        index += 1;
        while (index < lines.length && !/^\s*```\s*$/.test(lines[index])) {
          codeLines.push(lines[index]);
          index += 1;
        }
        if (index < lines.length) index += 1;
        const language = /^[\w.+-]{1,40}$/.test(fence[1] || "") ? fence[1] : "";
        fragment.appendChild(codeBlock(language, codeLines.join("\n")));
        continue;
      }
      const video = videoSourceFor(line);
      if (video) {
        fragment.appendChild(videoNode(video));
        index += 1;
        continue;
      }
      if (/^\s*(\$\$|\\\[)/.test(line)) {
        const math = matchMathBlock(lines, index);
        if (math) {
          const wrapper = document.createElement("div");
          wrapper.className = "math-block";
          renderMathInto(wrapper, math.tex, true);
          fragment.appendChild(wrapper);
          index = math.nextIndex;
          continue;
        }
      }
      if (isTableStart(lines, index)) {
        const rendered = markdownTable(lines, index);
        fragment.appendChild(rendered.node);
        index = rendered.nextIndex;
        continue;
      }
      if (isHorizontalRule(line)) {
        fragment.appendChild(document.createElement("hr"));
        index += 1;
        continue;
      }
      const heading = line.match(/^(#{1,6})\s+(.+)$/);
      if (heading) {
        const level = Math.min(6, heading[1].length + 1);
        const node = document.createElement(`h${level}`);
        appendInline(node, heading[2]);
        fragment.appendChild(node);
        index += 1;
        continue;
      }
      const unordered = line.match(/^\s*[-*+]\s+(.+)$/);
      if (unordered) {
        const list = document.createElement("ul");
        let hasTask = false;
        while (index < lines.length) {
          const itemMatch = lines[index].match(/^\s*[-*+]\s+(.+)$/);
          if (!itemMatch) break;
          const item = document.createElement("li");
          const task = itemMatch[1].match(/^\[([ xX])\]\s+(.*)$/);
          if (task) {
            hasTask = true;
            item.className = "task-list-item";
            const checkbox = document.createElement("input");
            checkbox.type = "checkbox";
            checkbox.checked = task[1].toLowerCase() === "x";
            checkbox.disabled = true;
            const content = document.createElement("span");
            appendInline(content, task[2]);
            item.append(checkbox, content);
          } else {
            appendInline(item, itemMatch[1]);
          }
          list.appendChild(item);
          index += 1;
        }
        if (hasTask) list.classList.add("task-list");
        fragment.appendChild(list);
        continue;
      }
      const ordered = line.match(/^\s*\d+[.)]\s+(.+)$/);
      if (ordered) {
        const list = document.createElement("ol");
        while (index < lines.length) {
          const itemMatch = lines[index].match(/^\s*\d+[.)]\s+(.+)$/);
          if (!itemMatch) break;
          const item = document.createElement("li");
          appendInline(item, itemMatch[1]);
          list.appendChild(item);
          index += 1;
        }
        fragment.appendChild(list);
        continue;
      }
      if (/^\s*>/.test(line)) {
        const quoteLines = [];
        while (index < lines.length) {
          const quote = lines[index].match(/^\s*>\s?(.*)$/);
          if (!quote) break;
          quoteLines.push(quote[1]);
          index += 1;
        }
        const blockquote = document.createElement("blockquote");
        appendInline(blockquote, quoteLines.join("\n"));
        fragment.appendChild(blockquote);
        continue;
      }
      const paragraphLines = [line];
      index += 1;
      while (index < lines.length && lines[index].trim() && !isMarkdownBlockStart(lines, index)) {
        paragraphLines.push(lines[index]);
        index += 1;
      }
      const paragraph = document.createElement("p");
      appendInline(paragraph, paragraphLines.join("\n"));
      fragment.appendChild(paragraph);
    }
    container.replaceChildren(fragment);
  }

  function createDayDivider(timestamp) {
    const divider = document.createElement("div");
    divider.className = "day-divider";
    divider.dataset.dayKey = dayKey(timestamp);
    const label = document.createElement("span");
    label.textContent = formatDayLabel(timestamp);
    divider.appendChild(label);
    return divider;
  }

  function appendDayDividerIfNeeded(timestamp) {
    const dividers = elements.timeline.querySelectorAll(".day-divider");
    const lastDivider = dividers[dividers.length - 1];
    if (!lastDivider || lastDivider.dataset.dayKey !== dayKey(timestamp)) elements.timeline.appendChild(createDayDivider(timestamp));
  }

  function createUserMessage(content, timestamp, attributes = {}) {
    // 系统自动触发的后台任务跟进不是真实用户输入，渲染为居中系统事件而不是用户气泡。
    const rawContent = String(content || "");
    if (rawContent.startsWith("[后台任务完成]") || rawContent.startsWith("[后台命令完成]") || rawContent.startsWith("<background-job-report>")) {
      const notice = document.createElement("div");
      notice.className = "system-event";
      if (attributes.turnId) notice.dataset.turnId = attributes.turnId;
      const label = document.createElement("span");
      let labelText = "";
      if (rawContent.startsWith("[后台任务完成]")) {
        labelText = rawContent.replace(/^\[后台任务完成\]\s*/, "");
      } else if (rawContent.startsWith("[后台命令完成]")) {
        const stripped = rawContent.replace(/^\[后台命令完成\]\s*/, "");
        labelText = `命令完成 ${stripped.split(" · ").slice(0, 2).join(" · ")}`;
      } else {
        const inner = (rawContent.match(/「(.*?)」/)?.[1] || "").trim();
        labelText = inner ? `任务完成 ${inner}` : "后台任务完成";
      }
      label.textContent = `⚙ ${labelText}`;
      label.title = rawContent;
      label.title = formatDateTime(timestamp);
      notice.appendChild(label);
      return notice;
    }
    const article = document.createElement("article");
    article.className = "message user-message";
    article.dataset.role = "user";
    if (attributes.turnId) article.dataset.turnId = attributes.turnId;
    if (attributes.runId) article.dataset.runId = attributes.runId;
    if (attributes.followupId) article.dataset.followupId = attributes.followupId;
    if (attributes.inputId) article.dataset.inputId = attributes.inputId;
    const bubble = document.createElement("div");
    bubble.className = "user-bubble";
    const paragraph = document.createElement("p");
    const textContent = String(content || "");
    paragraph.textContent = textContent;
    bubble.appendChild(paragraph);
    bubble.hidden = !textContent.trim();
    const attachments = createUserAttachments(attributes.attachments);
    const actions = document.createElement("div");
    actions.className = "message-actions";
    const time = document.createElement("span");
    time.textContent = formatTime(timestamp) || "刚刚";
    time.title = formatDateTime(timestamp);
    actions.appendChild(time);
    if (attributes.revisionTarget) {
      const edit = makeMessageAction("square-pen", "编辑最后一条消息", () => {
        openRevisionEditor(article, bubble, textContent, attributes.revisionTarget, edit);
      });
      edit.className = "edit-action";
      actions.appendChild(edit);
    }
    if (textContent.trim()) actions.appendChild(makeCopyButton(textContent, "复制消息"));
    if (attachments) article.appendChild(attachments);
    article.append(bubble, actions);
    return article;
  }

  function createUserAttachments(values) {
    const attachments = Array.isArray(values) ? values : [];
    if (!attachments.length) return null;
    const list = document.createElement("div");
    list.className = "user-attachments";
    for (const attachment of attachments) {
      const url = safeAttachmentUrl(attachment?.url);
      if (!url) continue;
      const name = String(attachment?.name || "附件");
      if (attachment?.kind === "image" || String(attachment?.mime || "").startsWith("image/")) {
        const link = document.createElement("a");
        link.className = "user-attachment-image";
        link.href = url;
        link.target = "_blank";
        link.rel = "noopener noreferrer";
        link.title = name;
        const image = document.createElement("img");
        image.src = url;
        image.alt = name;
        image.loading = "lazy";
        image.decoding = "async";
        const width = validAssetDimension(attachment?.width);
        const height = validAssetDimension(attachment?.height);
        if (width) image.width = width;
        if (height) image.height = height;
        link.appendChild(image);
        list.appendChild(link);
        continue;
      }
      const link = document.createElement("a");
      link.className = "user-attachment-file";
      link.href = url;
      link.setAttribute("download", "");
      link.title = `下载 ${name}`;
      link.appendChild(makeIconSlot("file-text"));
      const copy = document.createElement("span");
      const strong = document.createElement("strong");
      strong.textContent = name;
      const small = document.createElement("small");
      small.textContent = formatFileSize(attachment?.size);
      copy.append(strong, small);
      link.append(copy, makeIconSlot("download"));
      list.appendChild(link);
    }
    return list.childElementCount ? list : null;
  }

  function safeAssetUrl(value) {
    const raw = String(value || "").trim();
    if (!raw) return null;
    try {
      const url = new URL(raw, window.location.origin);
      if (url.origin !== window.location.origin || !url.pathname.startsWith("/api/assets/") || url.pathname === "/api/assets/") return null;
      return url.href;
    } catch (_) {
      return null;
    }
  }

  function safeArtifactUrl(value) {
    const raw = String(value || "").trim();
    if (!raw) return null;
    try {
      const url = new URL(raw, window.location.origin);
      const allowed = ["/api/assets/", "/api/artifacts/"].some((prefix) => url.pathname.startsWith(prefix) && url.pathname !== prefix);
      return url.origin === window.location.origin && allowed ? url.href : null;
    } catch (_) {
      return null;
    }
  }

  function artifactName(source) {
    return String(source?.name || source?.alt || "预览资源").trim() || "预览资源";
  }

  function normalizeArtifact(source, fallbackKind = "file") {
    if (!source || typeof source !== "object") return null;
    const url = safeArtifactUrl(source.url);
    if (!url) return null;
    const mime = String(source.mime || "application/octet-stream").toLowerCase();
    return {
      ...source,
      id: String(source.id || url),
      url,
      name: artifactName(source),
      type_label: String(source.type_label || "").trim().toUpperCase(),
      mime,
      kind: String(source.kind || (mime.startsWith("image/") ? "image" : fallbackKind))
    };
  }

  function artifactSupportsPreview(artifact) {
    return artifact?.kind === "image"
      || artifact?.mime?.startsWith("image/")
      || ["markdown", "html", "pdf"].includes(artifact?.kind);
  }

  function artifactSupportsSource(artifact) {
    return ["markdown", "html", "text", "code", "json"].includes(artifact?.kind)
      || artifact?.mime?.startsWith("text/")
      || artifact?.mime?.startsWith("application/json");
  }

  function defaultArtifactMode(artifact) {
    return artifactSupportsPreview(artifact) ? "preview" : "source";
  }

  function artifactWidthPixels() {
    const viewportWidth = Math.max(320, layoutViewportWidth());
    return Math.min(viewportWidth - 20, Math.max(320, viewportWidth * state.artifactWidthRatio));
  }

  function syncArtifactLayout() {
    const width = artifactWidthPixels();
    elements.mainStage.style.setProperty("--artifact-width", `${Math.round(width)}px`);
    const roomForConversation = elements.mainStage.clientWidth - width - 10;
    const split = state.artifactOpen && !state.artifactMaximized && layoutViewportWidth() > 760 && roomForConversation >= 320;
    elements.mainStage.classList.toggle("artifact-split", split);
    elements.mainStage.classList.toggle("artifact-maximized", state.artifactOpen && state.artifactMaximized);
    syncSidebarSpace();
  }

  function closeArtifactResourceMenu() {
    elements.artifactResourceMenu.hidden = true;
    elements.artifactTitleButton.setAttribute("aria-expanded", "false");
  }

  function setArtifactWorkspaceOpen(open) {
    const hasArtifacts = state.artifacts.length > 0;
    state.artifactOpen = Boolean(open && hasArtifacts);
    if (!state.artifactOpen) state.artifactMaximized = false;
    elements.artifactWorkspace.hidden = !state.artifactOpen;
    elements.artifactWorkspace.setAttribute("aria-hidden", String(!state.artifactOpen));
    elements.mainStage.classList.toggle("artifact-open", state.artifactOpen);
    closeArtifactResourceMenu();
    syncArtifactLayout();
    elements.artifactToggleButton.setAttribute("aria-pressed", String(state.artifactOpen));
    if (state.artifactOpen) {
      elements.artifactToggleButton.classList.remove("has-new-artifact");
      renderArtifactWorkspace();
    }
  }

  function registerArtifact(source, { autoOpen = false } = {}) {
    const artifact = normalizeArtifact(source, source?.kind || "file");
    if (!artifact) return;
    const index = state.artifacts.findIndex((item) => item.id === artifact.id);
    if (index >= 0) state.artifacts[index] = artifact;
    else state.artifacts.push(artifact);
    state.artifactSourceCache.delete(artifact.id);
    state.selectedArtifactId = artifact.id;
    state.artifactMode = defaultArtifactMode(artifact);
    state.artifactZoom = 1;
    state.artifactPanX = 0;
    state.artifactPanY = 0;
    elements.artifactToggleButton.hidden = false;
    if (autoOpen && layoutViewportWidth() > 760) setArtifactWorkspaceOpen(true);
    else if (!state.artifactOpen) elements.artifactToggleButton.classList.add("has-new-artifact");
    if (state.artifactOpen) renderArtifactWorkspace();
  }

  function syncArtifactsFromTurns(turns) {
    const artifacts = [];
    for (const turn of turns) {
      for (const source of [...(Array.isArray(turn?.assets) ? turn.assets : []), ...(Array.isArray(turn?.artifacts) ? turn.artifacts : [])]) {
        const artifact = normalizeArtifact(source, "file");
        if (artifact && !artifacts.some((item) => item.id === artifact.id)) artifacts.push(artifact);
      }
    }
    state.artifacts = artifacts;
    if (!artifacts.some((item) => item.id === state.selectedArtifactId)) {
      state.selectedArtifactId = artifacts.at(-1)?.id || null;
      state.artifactMode = defaultArtifactMode(artifacts.at(-1));
    }
    const knownIds = new Set(artifacts.map((artifact) => artifact.id));
    for (const id of state.artifactSourceCache.keys()) {
      if (!knownIds.has(id)) state.artifactSourceCache.delete(id);
    }
    elements.artifactToggleButton.hidden = artifacts.length === 0;
    if (!artifacts.length) setArtifactWorkspaceOpen(false);
    else if (state.artifactOpen) renderArtifactWorkspace();
    else if (window.location.hash.includes("artifact")) {
      // 深链 #artifact:载入后自动展开预览工作区(与 #console 同一约定)。
      window.location.hash = "";
      setArtifactWorkspaceOpen(true);
    }
  }

  function artifactIconName(artifact) {
    if (artifact?.kind === "image" || artifact?.mime?.startsWith("image/")) return "image";
    if (artifact?.kind === "markdown") return "file-markdown";
    if (artifact?.kind === "json") return "file-json";
    if (artifact?.kind === "code" || artifact?.kind === "html") return "file-code";
    return "file-text";
  }

  function artifactTypeLabel(artifact) {
    if (artifact?.type_label) return artifact.type_label;
    if (artifact?.kind === "markdown") return "MD";
    if (artifact?.kind === "json") return "JSON";
    if (artifact?.kind === "html") return "HTML";
    if (artifact?.kind === "code") return "CODE";
    if (artifact?.kind === "pdf") return "PDF";
    if (artifact?.kind === "image") return String(artifact.mime || "IMAGE").split("/").pop().toUpperCase();
    return "FILE";
  }

  function artifactIconButton(icon, label, handler) {
    const button = document.createElement("button");
    button.type = "button";
    button.className = "icon-button";
    button.title = label;
    button.setAttribute("aria-label", label);
    button.appendChild(makeIconSlot(icon));
    button.addEventListener("click", handler);
    return button;
  }

  function renderArtifactImage(artifact) {
    const stage = document.createElement("div");
    stage.className = "artifact-image-stage";
    const image = document.createElement("img");
    image.src = artifact.url;
    image.alt = artifact.name;
    const applyTransform = () => {
      image.style.transform = `translate(${state.artifactPanX}px, ${state.artifactPanY}px) scale(${state.artifactZoom})`;
      stage.classList.toggle("is-zoomed", state.artifactZoom > 1);
    };
    applyTransform();
    stage.addEventListener("wheel", (event) => {
      event.preventDefault();
      const nextZoom = Math.min(4, Math.max(0.25, state.artifactZoom * (event.deltaY < 0 ? 1.12 : 0.89)));
      state.artifactZoom = nextZoom;
      if (nextZoom <= 1) {
        state.artifactZoom = 1;
        state.artifactPanX = 0;
        state.artifactPanY = 0;
      }
      applyTransform();
      updateArtifactImageControls();
    }, { passive: false });
    stage.addEventListener("pointerdown", (event) => {
      if (state.artifactZoom <= 1 || event.button !== 0) return;
      event.preventDefault();
      stage.classList.add("is-dragging");
      stage.setPointerCapture(event.pointerId);
      stage.dataset.panStartX = String(event.clientX);
      stage.dataset.panStartY = String(event.clientY);
      stage.dataset.panOriginX = String(state.artifactPanX);
      stage.dataset.panOriginY = String(state.artifactPanY);
    });
    stage.addEventListener("pointermove", (event) => {
      if (!stage.classList.contains("is-dragging")) return;
      state.artifactPanX = Number(stage.dataset.panOriginX)
        + visualPixelsToLayout(event.clientX - Number(stage.dataset.panStartX));
      state.artifactPanY = Number(stage.dataset.panOriginY)
        + visualPixelsToLayout(event.clientY - Number(stage.dataset.panStartY));
      applyTransform();
    });
    const finishPan = () => stage.classList.remove("is-dragging");
    stage.addEventListener("pointerup", finishPan);
    stage.addEventListener("pointercancel", finishPan);
    stage.appendChild(image);
    return stage;
  }

  function updateArtifactImageControls() {
    const isImage = state.artifacts.find((item) => item.id === state.selectedArtifactId)?.kind === "image";
    if (!isImage) return;
    elements.artifactImageZoomOutButton.disabled = state.artifactZoom <= 0.25;
    elements.artifactImageZoomInButton.disabled = state.artifactZoom >= 4;
  }

  async function loadArtifactSource(artifact) {
    const version = `${artifact.url}|${artifact.updated_at || ""}`;
    const cached = state.artifactSourceCache.get(artifact.id);
    if (cached?.version === version) return cached.text;
    const response = await fetch(artifact.url, { credentials: "same-origin", cache: "no-store" });
    if (!response.ok) throw new Error("文件载入失败");
    const text = await response.text();
    state.artifactSourceCache.set(artifact.id, { version, text });
    return text;
  }

  function artifactLoadingNode() {
    const loading = document.createElement("div");
    loading.className = "artifact-loading";
    loading.append(makeIconSlot("loader-circle", "is-spinning"));
    return loading;
  }

  function renderArtifactFailure(error, token) {
    if (token !== state.artifactRenderToken) return;
    const failure = document.createElement("div");
    failure.className = "artifact-failure";
    failure.append(makeIconSlot("circle-alert"), document.createTextNode(error?.message || "文件载入失败"));
    elements.artifactView.replaceChildren(failure);
  }

  async function renderArtifactSource(artifact, token) {
    let text = await loadArtifactSource(artifact);
    if (token !== state.artifactRenderToken) return;
    if (artifact.kind === "json" || artifact.mime.startsWith("application/json") || /\.json$/i.test(artifact.name)) {
      try { text = JSON.stringify(JSON.parse(text), null, 2); } catch (_) {}
    }
    const source = document.createElement("div");
    source.className = "artifact-source";
    const gutter = document.createElement("div");
    gutter.className = "artifact-line-numbers";
    const lines = text.split("\n");
    for (let index = 0; index < lines.length; index += 1) {
      const number = document.createElement("span");
      number.textContent = String(index + 1);
      gutter.appendChild(number);
    }
    const pre = document.createElement("pre");
    pre.className = "artifact-code";
    const code = document.createElement("code");
    code.textContent = text;
    pre.appendChild(code);
    source.append(gutter, pre);
    elements.artifactView.replaceChildren(source);
  }

  async function renderArtifactPreview(artifact, token) {
    if (artifact.kind === "image" || artifact.mime.startsWith("image/")) {
      elements.artifactView.replaceChildren(renderArtifactImage(artifact));
      return;
    }
    if (artifact.kind === "pdf") {
      const frame = document.createElement("iframe");
      frame.className = "artifact-frame";
      frame.src = artifact.url;
      frame.title = artifact.name;
      elements.artifactView.replaceChildren(frame);
      return;
    }
    if (artifact.kind === "html") {
      const frame = document.createElement("iframe");
      frame.className = "artifact-frame";
      frame.src = artifact.url;
      frame.title = artifact.name;
      frame.setAttribute("sandbox", "");
      elements.artifactView.replaceChildren(frame);
      return;
    }
    if (artifact.kind === "markdown") {
      const text = await loadArtifactSource(artifact);
      if (token !== state.artifactRenderToken) return;
      const article = document.createElement("article");
      article.className = "markdown-body artifact-markdown";
      renderMarkdown(article, text);
      elements.artifactView.replaceChildren(article);
      return;
    }
    throw new Error("此格式不支持预览");
  }

  function renderArtifactResourceMenu(artifact) {
    elements.artifactResourceMenu.replaceChildren();
    for (const item of state.artifacts) {
      const button = document.createElement("button");
      button.type = "button";
      button.role = "menuitem";
      button.className = item.id === artifact.id ? "active" : "";
      const label = document.createElement("span");
      label.textContent = item.name;
      const type = document.createElement("small");
      type.textContent = artifactTypeLabel(item);
      button.append(makeIconSlot(artifactIconName(item)), label, type);
      if (item.id === artifact.id) button.appendChild(makeIconSlot("check"));
      button.addEventListener("click", () => {
        state.selectedArtifactId = item.id;
        state.artifactMode = defaultArtifactMode(item);
        state.artifactZoom = 1;
        state.artifactPanX = 0;
        state.artifactPanY = 0;
        closeArtifactResourceMenu();
        renderArtifactWorkspace();
      });
      elements.artifactResourceMenu.appendChild(button);
    }
    elements.artifactTitleButton.disabled = state.artifacts.length <= 1;
  }

  function renderArtifactWorkspace() {
    if (!state.artifactOpen) return;
    const artifact = state.artifacts.find((item) => item.id === state.selectedArtifactId) || state.artifacts.at(-1);
    if (!artifact) return;
    state.selectedArtifactId = artifact.id;
    const canPreview = artifactSupportsPreview(artifact);
    const canSource = artifactSupportsSource(artifact);
    const isImage = artifact.kind === "image" || artifact.mime.startsWith("image/");
    if ((state.artifactMode === "preview" && !canPreview) || (state.artifactMode === "source" && !canSource)) {
      state.artifactMode = defaultArtifactMode(artifact);
    }
    elements.artifactTitle.textContent = artifact.name;
    elements.artifactTitle.title = artifact.name;
    elements.artifactTypeLabel.textContent = artifactTypeLabel(artifact);
    elements.artifactDownloadButton.href = artifact.url;
    elements.artifactPreviewButton.parentElement.hidden = isImage;
    elements.artifactImageActions.hidden = !isImage;
    elements.artifactImageExternalButton.href = isImage ? artifact.url : "";
    elements.artifactImageZoomOutButton.disabled = !isImage || state.artifactZoom <= 0.25;
    elements.artifactImageZoomInButton.disabled = !isImage || state.artifactZoom >= 4;
    elements.artifactPreviewButton.hidden = !canPreview;
    elements.artifactSourceButton.hidden = !canSource;
    elements.artifactPreviewButton.classList.toggle("active", state.artifactMode === "preview");
    elements.artifactSourceButton.classList.toggle("active", state.artifactMode === "source");
    elements.artifactPreviewButton.setAttribute("aria-pressed", String(state.artifactMode === "preview"));
    elements.artifactSourceButton.setAttribute("aria-pressed", String(state.artifactMode === "source"));
    elements.artifactCopyButton.disabled = !canSource && artifact.kind === "pdf";
    elements.artifactCopyButton.hidden = isImage;
    elements.artifactMaximizeButton.replaceChildren(makeIconSlot(state.artifactMaximized ? "minimize-2" : "maximize-2"));
    elements.artifactMaximizeButton.title = state.artifactMaximized ? "退出全屏" : "全屏显示";
    elements.artifactMaximizeButton.setAttribute("aria-label", elements.artifactMaximizeButton.title);
    renderArtifactResourceMenu(artifact);
    const token = ++state.artifactRenderToken;
    elements.artifactView.replaceChildren(artifactLoadingNode());
    const render = state.artifactMode === "source"
      ? renderArtifactSource(artifact, token)
      : renderArtifactPreview(artifact, token);
    render.catch((error) => renderArtifactFailure(error, token));
  }

  async function copySelectedArtifact() {
    const artifact = state.artifacts.find((item) => item.id === state.selectedArtifactId);
    if (!artifact) return;
    try {
      if (artifactSupportsSource(artifact)) {
        await navigator.clipboard.writeText(await loadArtifactSource(artifact));
      } else if (artifact.kind === "image" && window.ClipboardItem) {
        const response = await fetch(artifact.url, { credentials: "same-origin" });
        if (!response.ok) throw new Error("图片载入失败");
        const blob = await response.blob();
        await navigator.clipboard.write([new ClipboardItem({ [blob.type]: blob })]);
      } else {
        await navigator.clipboard.writeText(artifact.url);
      }
      showToast("已复制", "success");
    } catch (error) {
      showToast(error.message || "复制失败", "error");
    }
  }

  function setArtifactMode(mode) {
    const artifact = state.artifacts.find((item) => item.id === state.selectedArtifactId);
    if (!artifact || (mode === "preview" ? !artifactSupportsPreview(artifact) : !artifactSupportsSource(artifact))) return;
    state.artifactMode = mode;
    renderArtifactWorkspace();
  }

  function toggleArtifactMaximized() {
    if (!state.artifactOpen) return;
    state.artifactMaximized = !state.artifactMaximized;
    syncArtifactLayout();
    renderArtifactWorkspace();
  }

  function changeArtifactImageZoom(delta) {
    const artifact = state.artifacts.find((item) => item.id === state.selectedArtifactId);
    if (!artifact || !(artifact.kind === "image" || artifact.mime.startsWith("image/"))) return;
    state.artifactZoom = Math.min(4, Math.max(0.25, (state.artifactZoom || 1) + delta));
    if (state.artifactZoom <= 1) {
      state.artifactZoom = 1;
      state.artifactPanX = 0;
      state.artifactPanY = 0;
    }
    const image = elements.artifactView.querySelector(".artifact-image-stage > img");
    if (image) {
      image.style.transform = `translate(${state.artifactPanX}px, ${state.artifactPanY}px) scale(${state.artifactZoom})`;
      image.closest(".artifact-image-stage")?.classList.toggle("is-zoomed", state.artifactZoom > 1);
    }
    updateArtifactImageControls();
  }

  function validAssetDimension(value) {
    const number = Number(value);
    return Number.isInteger(number) && number > 0 && number <= 100_000 ? number : null;
  }

  function createAssetAction(iconName, label, href, download = false) {
    const link = document.createElement("a");
    link.href = href;
    link.title = label;
    link.setAttribute("aria-label", label);
    link.rel = "noopener noreferrer";
    if (download) link.setAttribute("download", "");
    else link.target = "_blank";
    link.appendChild(makeIconSlot(iconName));
    return link;
  }

  function createConversationMedia(asset, { eager = false } = {}) {
    const source = asset && typeof asset === "object" ? asset : {};
    const url = safeAssetUrl(source.url);
    const mime = String(source.mime || "").trim().toLowerCase();
    const imageMime = !mime || mime.startsWith("image/");
    const width = validAssetDimension(source.width);
    const height = validAssetDimension(source.height);
    const alt = String(source.alt || "").trim() || "Miyu 生成的图片";
    const hideCaption = Boolean(source.hide_caption);

    const figure = document.createElement("figure");
    figure.className = "conversation-media";
    if (source.id != null) figure.dataset.assetId = String(source.id);
    const visual = document.createElement("div");
    visual.className = "conversation-media-visual";
    if (width && height) {
      const ratio = width / height;
      if (ratio >= 0.05 && ratio <= 20) {
        visual.classList.add("has-aspect");
        visual.style.aspectRatio = `${width} / ${height}`;
      }
    }
    const fallback = document.createElement("div");
    fallback.className = "conversation-media-fallback";
    fallback.appendChild(makeIconSlot("circle-alert"));
    const fallbackText = document.createElement("span");
    fallbackText.textContent = url && imageMime ? "图片载入失败" : "图片地址不可用";
    fallback.appendChild(fallbackText);

    if (url && imageMime) {
      const image = document.createElement("img");
      image.alt = alt;
      image.loading = eager ? "eager" : "lazy";
      image.decoding = "async";
      if (width) image.width = width;
      if (height) image.height = height;
      fallback.hidden = true;
      image.addEventListener("error", () => {
        image.remove();
        fallback.hidden = false;
        figure.classList.add("is-error");
        contentAdded();
      }, { once: true });
      image.addEventListener("load", contentAdded, { once: true });
      image.src = url;
      visual.append(image, fallback);
    } else {
      visual.appendChild(fallback);
    }

    const caption = document.createElement("figcaption");
    caption.className = "conversation-media-caption";
    if (!hideCaption) {
      const captionText = document.createElement("span");
      captionText.textContent = alt;
      captionText.title = alt;
      caption.appendChild(captionText);
    } else {
      caption.classList.add("is-actions-only");
    }
    if (url) {
      const actions = document.createElement("span");
      actions.className = "conversation-media-actions";
      actions.append(
        artifactIconButton("panel-right", "在预览工作区打开", () => {
          registerArtifact({ ...source, url, name: alt, kind: "image" });
          setArtifactWorkspaceOpen(true);
        }),
        createAssetAction("external-link", "在新窗口打开图片", url),
        createAssetAction("download", "下载图片", url, true)
      );
      caption.appendChild(actions);
    }
    figure.appendChild(visual);
    if (caption.childElementCount) figure.appendChild(caption);
    return figure;
  }

  /*
   * display.reasoning 只决定后端产生什么(摘要/完整/不产生);
   * WebUI 是否渲染仅以「有没有思考内容」为准,hidden 时若仍收到文本则不渲染(保底)。
   * 默认展开/收起由本地偏好 miyu.web.reasoningExpanded 决定,与 summary/full 无关。
   */
  function reasoningHidden() {
    return state.display?.reasoning === "hidden";
  }

  function normalizeReasoningTitle(value) {
    const title = String(value || "").trim().replace(/^[*#\s]+|[*#\s]+$/g, "");
    if (!title || /^正在(?:思考)?(?:\.{3}|…+)?$/u.test(title)) return "";
    return title;
  }

  function splitReasoningText(value) {
    const raw = String(value || "").trim();
    const bold = raw.match(/^\*\*([^\n*]{1,160})\*\*(?:\r?\n){0,2}([\s\S]*)$/);
    if (bold) return { title: normalizeReasoningTitle(bold[1]), body: bold[2].trim() };
    const heading = raw.match(/^#{1,6}\s+([^\n]{1,160})(?:\r?\n)+([\s\S]*)$/);
    if (heading) return { title: normalizeReasoningTitle(heading[1]), body: heading[2].trim() };
    return { title: "", body: raw };
  }

  function createReasoningBlock(text, title = "已思考", live = false, summaryOnly = false) {
    const details = document.createElement("details");
    details.className = "reasoning-block";
    details.classList.toggle("is-summary", summaryOnly);
    details.classList.toggle("is-live", live);
    details.open = state.reasoningExpanded === true;
    const summary = document.createElement("summary");
    const atom = makeIconSlot("atom", "reasoning-icon");
    if (live) for (let index = 0; index < 3; index += 1) atom.appendChild(document.createElement("i"));
    const titleNode = document.createElement("span");
    titleNode.className = "reasoning-title";
    titleNode.textContent = title || (live ? "正在思考" : "已思考");
    const chevron = makeIconSlot("chevron-right", "reasoning-chevron");
    summary.append(atom, titleNode);
    let liveStatus = null;
    let progress = null;
    if (live) {
      liveStatus = document.createElement("span");
      liveStatus.className = "reasoning-live-status";
      liveStatus.textContent = "0s";
      summary.appendChild(liveStatus);
      progress = document.createElement("div");
      progress.className = "reasoning-progress";
      progress.setAttribute("role", "progressbar");
      progress.setAttribute("aria-label", "思考进度");
      progress.setAttribute("aria-valuetext", "正在思考");
      const progressFill = document.createElement("i");
      progressFill.setAttribute("aria-hidden", "true");
      progress.appendChild(progressFill);
    }
    summary.appendChild(chevron);
    const body = document.createElement("div");
    body.className = "reasoning-text";
    body.textContent = String(text || "");
    details.append(summary);
    if (progress) details.appendChild(progress);
    details.appendChild(body);
    const block = {
      element: details,
      title: titleNode,
      liveStatus,
      progress,
      body,
      raw: String(text || ""),
      pendingTitle: "",
      summaryOnly,
      partOpen: false,
      startedAt: live ? performance.now() : null,
      finished: !live,
      userToggled: false,
      ignoreNextToggle: false
    };
    details.addEventListener("toggle", () => {
      if (block.ignoreNextToggle) {
        block.ignoreNextToggle = false;
        return;
      }
      block.userToggled = true;
    });
    return block;
  }

  function createAssistantMessage({
    content = "",
    reasoning = "",
    reasoningTitle = "已思考",
    assets = [],
    timestamp = null,
    tokenTotal = 0,
    tokenPrompt = 0,
    tokenCached = 0,
    tokenEstimated = false,
    providerId = "",
    model = "",
    activeContext = true,
    turnId = null,
    muted = false,
    segmentKind = "final",
    redoTarget = null
  } = {}) {
    const article = document.createElement("article");
    article.className = `message assistant-message${muted ? " is-muted" : ""}`;
    article.dataset.role = "assistant";
    if (turnId) article.dataset.turnId = turnId;
    article.dataset.segmentKind = segmentKind;
    const header = document.createElement("header");
    header.className = "assistant-label";
    const avatar = document.createElement("img");
    avatar.alt = "";
    avatar.setAttribute("aria-hidden", "true");
    setPersonaAvatar(avatar);
    const identity = document.createElement("div");
    const name = document.createElement("strong");
    name.textContent = state.persona.name;
    const time = document.createElement("span");
    time.textContent = formatTime(timestamp) || "";
    time.title = formatDateTime(timestamp);
    identity.append(name, time);
    header.append(avatar, identity);
    const assistantContent = document.createElement("div");
    assistantContent.className = "assistant-content";
    const blocks = document.createElement("div");
    blocks.className = "assistant-blocks";
    if (String(reasoning || "").trim() && !reasoningHidden()) {
      const parsed = splitReasoningText(reasoning);
      blocks.appendChild(createReasoningBlock(parsed.body, "已思考", false).element);
    }
    if (String(content || "").trim()) {
      const markdown = document.createElement("div");
      markdown.className = "markdown-body";
      renderMarkdown(markdown, content);
      blocks.appendChild(markdown);
    }
    for (const asset of Array.isArray(assets) ? assets : []) blocks.appendChild(createConversationMedia(asset));
    assistantContent.appendChild(blocks);
    assistantContent.classList.toggle("is-slim", !blocks.querySelector(WIDE_BLOCK_SELECTOR));
    article.append(header, assistantContent);

    const meta = document.createElement("div");
    meta.className = "assistant-meta";
    if (state.display?.show_mixed_model_endpoint && (String(providerId || "").trim() || String(model || "").trim())) {
      const endpoint = document.createElement("span");
      endpoint.className = "assistant-endpoint";
      endpoint.textContent = [providerId, model].map((value) => String(value || "").trim()).filter(Boolean).join(" / ");
      meta.appendChild(endpoint);
    }
    const usageText = formatUsageMeta({
      turnTotal: tokenTotal,
      turnPrompt: tokenPrompt,
      turnCached: tokenCached,
      estimated: tokenEstimated
    });
    if (usageText) {
      const token = document.createElement("span");
      token.textContent = usageText;
      meta.appendChild(token);
    }
    if (!activeContext) {
      const contextBadge = document.createElement("span");
      contextBadge.className = "context-state-badge";
      contextBadge.textContent = "已移出当前上下文";
      meta.appendChild(contextBadge);
    }
    const copyValue = String(content || "").trim() || String(reasoning || "");
    if (copyValue || redoTarget) {
      const spacer = document.createElement("span");
      spacer.className = "meta-spacer";
      meta.appendChild(spacer);
      if (redoTarget) {
        const redo = makeMessageAction("refresh-cw", "重新生成回复", () => submitRedo(redoTarget));
        redo.className = "redo-action";
        meta.appendChild(redo);
      }
      if (copyValue) meta.appendChild(makeCopyButton(copyValue, "复制回复"));
    }
    if (meta.childNodes.length) article.appendChild(meta);
    return article;
  }

  function setAssistantRedoAction(article, candidate) {
    const meta = article?.querySelector(".assistant-meta");
    if (!meta) return;
    meta.querySelector(".redo-action")?.remove();
    if (!candidate) return;
    const redo = makeMessageAction("refresh-cw", "重新生成回复", () => submitRedo(candidate));
    redo.className = "redo-action";
    const copy = meta.querySelector("button:last-child");
    if (copy) meta.insertBefore(redo, copy);
    else meta.appendChild(redo);
  }

  function createAnsweredQuestionCard(exchange, compact = true) {
    const card = document.createElement("section");
    card.className = "answered-question-card";
    if (compact) card.classList.add("is-compact");
    const header = document.createElement("header");
    const icon = document.createElement("span");
    icon.className = "question-icon";
    icon.appendChild(makeIconSlot("check"));
    const copy = document.createElement("div");
    const status = document.createElement("small");
    status.textContent = "已回答";
    const title = document.createElement("strong");
    const questions = Array.isArray(exchange?.questions) ? exchange.questions : [];
    title.textContent = questions.length === 1 ? String(questions[0]?.header || "补充确认") : `${questions.length} 项补充确认`;
    copy.append(status, title);
    header.append(icon, copy);
    const list = document.createElement("dl");
    list.className = "answered-question-list";
    const answers = Array.isArray(exchange?.answers) ? exchange.answers : [];
    questions.forEach((question, index) => {
      const row = document.createElement("div");
      const term = document.createElement("dt");
      term.textContent = String(question?.question || question?.header || `问题 ${index + 1}`);
      const description = document.createElement("dd");
      const selected = Array.isArray(answers[index]) ? answers[index] : [];
      description.textContent = selected.map(String).join("、") || "未记录";
      row.append(term, description);
      list.appendChild(row);
    });
    card.append(header, list);
    return card;
  }

  function createPersistedQuestion(exchange, turnId) {
    const wrapper = document.createElement("article");
    wrapper.className = "persisted-question-wrap";
    if (turnId) wrapper.dataset.turnId = turnId;
    wrapper.appendChild(createAnsweredQuestionCard(exchange));
    return wrapper;
  }

  function createTurnStatus(turn) {
    const status = document.createElement("div");
    status.className = "turn-status-line";
    status.dataset.turnStatus = String(turn?.id || "");
    const isInterrupted = turn?.status === "interrupted";
    status.classList.toggle("is-interrupted", isInterrupted);
    status.appendChild(makeIconSlot(isInterrupted ? "circle-alert" : "loader-circle"));
    const text = document.createElement("span");
    text.textContent = isInterrupted ? "本轮已中断" : "本轮正在运行";
    status.appendChild(text);
    if (asFiniteNumber(turn?.token_total) > 0) {
      const usage = document.createElement("span");
      usage.textContent = `${turn.token_usage_estimated ? "约 " : ""}${formatTokens(turn.token_total)} tokens`;
      status.appendChild(usage);
    }
    if (turn?.active_context === false) {
      const context = document.createElement("span");
      context.className = "context-state-badge";
      context.textContent = "已移出当前上下文";
      status.appendChild(context);
    }
    return status;
  }

  function renderPersistedTurn(turn) {
    const turnId = String(turn?.id || "");
    const candidate = state.redoCandidate && String(state.redoCandidate.turn_id) === turnId
      ? state.redoCandidate
      : null;
    elements.timeline.appendChild(createUserMessage(turn?.user_content || "", turn?.user_timestamp, {
      turnId,
      inputId: turnId,
      revisionTarget: candidate && String(candidate.input_id) === turnId ? candidate : null,
      attachments: turn?.attachments
    }));

    /*
     * 本页会话内完成的 turn:优先复用 live 流式渲染出的 article(含按时序排列的
     * 思考签 / 工具签 / 正文块),避免用扁平的「单 reasoning + 正文」重建而丢失时序。
     * 历史重载(后端快照没有 parts 顺序)才退回扁平重建。
     */
    const stash = turnId && turn?.status !== "running" ? state.finishedTurnArticles.get(turnId) : null;
    let stashIndex = 0;
    const takeStash = (kind) => {
      if (!stash || stashIndex >= stash.length || stash[stashIndex].kind !== kind) return null;
      return stash[stashIndex++].article;
    };

    // 已回答的问题卡在 live article 内部原位保留;仅在无存档时用快照重建。
    if (!stash) {
      const exchanges = Array.isArray(turn?.question_exchanges) ? turn.question_exchanges : [];
      for (const exchange of exchanges) elements.timeline.appendChild(createPersistedQuestion(exchange, turnId));
    }

    const followups = Array.isArray(turn?.followups) ? turn.followups : [];
    for (const followup of followups) {
      const precedingContent = String(followup?.preceding_assistant_content || "");
      const precedingReasoning = String(followup?.preceding_assistant_reasoning || "");
      const stashedSegment = takeStash("segment");
      if (stashedSegment) {
        elements.timeline.appendChild(stashedSegment);
      } else if (precedingContent.trim() || precedingReasoning.trim()) {
        elements.timeline.appendChild(createAssistantMessage({
          content: precedingContent,
          reasoning: precedingReasoning,
          providerId: followup?.provider_id,
          model: followup?.model,
          timestamp: followup?.submitted_at,
          turnId,
          segmentKind: "segment",
          activeContext: turn?.active_context !== false
        }));
      }
      elements.timeline.appendChild(createUserMessage(followup?.content || "", followup?.submitted_at, {
        turnId,
        followupId: String(followup?.id || ""),
        inputId: String(followup?.id || ""),
        revisionTarget: candidate && String(candidate.input_id) === String(followup?.id || "") ? candidate : null,
        attachments: followup?.attachments
      }));
    }
    let leftoverSegment;
    while ((leftoverSegment = takeStash("segment"))) elements.timeline.appendChild(leftoverSegment);

    const assistantContent = String(turn?.assistant_content || "");
    const assistantReasoning = String(turn?.assistant_reasoning || "");
    const assets = turn?.status === "running" ? [] : (Array.isArray(turn?.assets) ? turn.assets : []);
    const stashedFinal = takeStash("final");
    if (stashedFinal) {
      stashedFinal.classList.toggle("is-muted", turn?.active_context === false);
      stashedFinal.dataset.segmentKind = "final";
      setAssistantRedoAction(stashedFinal, candidate);
      elements.timeline.appendChild(stashedFinal);
    } else if (assistantContent.trim() || assistantReasoning.trim() || assets.length) {
      elements.timeline.appendChild(createAssistantMessage({
        content: assistantContent,
        reasoning: assistantReasoning,
        providerId: turn?.provider_id,
        model: turn?.model,
        assets,
        timestamp: turn?.assistant_timestamp,
        tokenTotal: turn?.token_total,
        tokenPrompt: turn?.token_prompt,
        tokenCached: turn?.token_cache_read,
        tokenEstimated: Boolean(turn?.token_usage_estimated),
        activeContext: turn?.active_context !== false,
        turnId,
        segmentKind: "final",
        redoTarget: candidate,
        muted: turn?.active_context === false
      }));
    }
    if (turn?.status === "running" || turn?.status === "interrupted") elements.timeline.appendChild(createTurnStatus(turn));
    else if (!stashedFinal && !assistantContent.trim() && !assistantReasoning.trim() && (asFiniteNumber(turn?.token_total) > 0 || turn?.active_context === false)) {
      const metadata = createTurnStatus({ ...turn, status: "completed" });
      metadata.querySelector("span:nth-child(2)").textContent = "本轮已完成";
      metadata.querySelector(".icon-slot").replaceChildren(createIcon("check"));
      elements.timeline.appendChild(metadata);
    }
  }

  function renderConversation() {
    elements.loadingState.hidden = true;
    elements.blockedState.hidden = true;
    clearQuestionDock();
    elements.timeline.replaceChildren();
    const turns = [...state.turns].sort((left, right) => asFiniteNumber(left?.seq) - asFiniteNumber(right?.seq));
    state.turns = turns;
    syncArtifactsFromTurns(turns);
    if (state.finishedTurnArticles.size) {
      const knownTurnIds = new Set(turns.map((turn) => String(turn?.id)));
      for (const key of [...state.finishedTurnArticles.keys()]) {
        if (!knownTurnIds.has(key)) state.finishedTurnArticles.delete(key);
      }
    }
    if (turns.length === 0) {
      elements.timeline.hidden = true;
      elements.emptyState.hidden = false;
    } else {
      elements.emptyState.hidden = true;
      elements.timeline.hidden = false;
      let previousDay = null;
      for (const turn of turns) {
        const currentDay = dayKey(turn?.user_timestamp);
        if (currentDay !== previousDay) {
          elements.timeline.appendChild(createDayDivider(turn?.user_timestamp));
          previousDay = currentDay;
        }
        renderPersistedTurn(turn);
      }
    }
    state.nearBottom = true;
    state.followOutput = true;
    elements.jumpBottomButton.hidden = true;
    updateConversationChrome();
    window.requestAnimationFrame(() => {
      elements.chatScroll.scrollTop = elements.chatScroll.scrollHeight;
    });
  }

  function createLiveState(runId, options = {}) {
    return {
      runId,
      turnId: options.turnId || null,
      userText: options.userText || "",
      userAttachments: Array.isArray(options.userAttachments) ? options.userAttachments : [],
      startedAt: options.startedAt || new Date(),
      userRendered: Boolean(options.userRendered),
      article: null,
      blocks: null,
      headerStatus: null,
      stopButton: null,
      cancellationRequested: false,
      meta: null,
      endpoint: null,
      copyButton: null,
      currentText: null,
      assistantText: "",
      assistantReasoning: "",
      assets: [],
      artifacts: [],
      reasoning: null,
      reasoningParts: [],
      reasoningStarted: false,
      reasoningTitle: "",
      reasoningTimer: null,
      providerId: "",
      model: "",
      tools: new Map(),
      preparingTool: null,
      questions: new Map(),
      contextOperation: null,
      typing: null,
      typingAnimation: null,
      streamRail: null,
      ended: false,
      operation: options.operation || "create",
      inputId: options.inputId || null,
      editedContent: options.editedContent ?? null,
      redoCommitted: false
    };
  }

  function isJobFollowupContent(content) {
    const raw = String(content || "");
    return raw.startsWith("[后台任务完成]") || raw.startsWith("[后台命令完成]") || raw.startsWith("<background-job-report>");
  }

  function renderQueueTray() {
    // 后台任务完成的自动跟进不是用户消息，不在排队托盘里显示。
    const prompts = (Array.isArray(state.queuedPrompts) ? state.queuedPrompts : [])
      .filter((prompt) => !isJobFollowupContent(prompt?.content) && !isJobFollowupContent(prompt?.display_content));
    elements.queueTray.replaceChildren();
    elements.queueTray.hidden = prompts.length === 0;
    for (const prompt of prompts) {
      const row = document.createElement("div");
      row.className = "queue-item";
      const text = document.createElement("span");
      const attachmentCount = Array.isArray(prompt?.attachments) ? prompt.attachments.length : 0;
      const promptText = String(prompt?.content || "").trim();
      text.textContent = attachmentCount
        ? `${promptText || "附件消息"} · ${attachmentCount} 个附件`
        : promptText;
      const remove = document.createElement("button");
      remove.type = "button";
      remove.className = "queue-remove";
      remove.title = "移除排队消息";
      remove.setAttribute("aria-label", "移除排队消息");
      remove.appendChild(makeIconSlot("x"));
      remove.addEventListener("click", () => removeQueuedPrompt(prompt.id));
      row.append(text, remove);
      elements.queueTray.appendChild(row);
    }
    updateControlState();
  }

  async function removeQueuedPrompt(promptId) {
    if (!promptId) return;
    const target = activeTurnUpdateTarget(state.viewSessionId);
    if (!target) {
      showToast("无法确定排队消息所属的回复", "error");
      return;
    }
    try {
      await apiRequest(`/api/runs/${encodeURIComponent(target.runId)}/turns/${encodeURIComponent(target.turnId)}/queue/${encodeURIComponent(promptId)}`, { method: "DELETE" });
      state.queuedPrompts = state.queuedPrompts.filter((prompt) => String(prompt?.id) !== String(promptId));
      renderQueueTray();
    } catch (error) {
      showToast(error.message || "排队消息移除失败", "error");
      if (error.status === 404 && state.viewSessionId) await loadSessionView(state.viewSessionId, { quiet: true });
    }
  }

  function disposeLiveState(live) {
    if (!live) return;
    for (const question of live.questions?.values?.() || []) {
      if (question.autoAdvanceTimer) window.clearTimeout(question.autoAdvanceTimer);
      question.autoAdvanceTimer = null;
    }
    clearPreparingTool(live);
    removeLiveStopButton(live);
    live.typingAnimation?.cancel();
    live.typingAnimation = null;
    if (live.reasoningTimer) {
      window.clearInterval(live.reasoningTimer);
      live.reasoningTimer = null;
    }
    if (live.currentText?.renderFrame) {
      window.cancelAnimationFrame(live.currentText.renderFrame);
      live.currentText.renderFrame = null;
    }
    for (const tool of live.tools?.values?.() || []) {
      if (tool.collapseTimer) window.clearTimeout(tool.collapseTimer);
      tool.collapseTimer = null;
      if (tool.outputRenderFrame) window.cancelAnimationFrame(tool.outputRenderFrame);
      tool.outputRenderFrame = null;
    }
  }

  function ensureTimelineVisible() {
    elements.loadingState.hidden = true;
    elements.blockedState.hidden = true;
    elements.emptyState.hidden = true;
    elements.timeline.hidden = false;
  }

  function ensureLiveUser(live, content) {
    if (!live || live.userRendered) return;
    const text = String(content || live.userText || "");
    if (!text.trim() && !live.userAttachments.length) return;
    live.userText = text;
    ensureTimelineVisible();
    appendDayDividerIfNeeded(new Date());
    const message = createUserMessage(text, new Date(), {
      runId: live.runId,
      attachments: live.userAttachments
    });
    if (live.article?.isConnected) elements.timeline.insertBefore(message, live.article);
    else elements.timeline.appendChild(message);
    live.userRendered = true;
    updateConversationChrome();
    contentAdded();
  }

  function removeRunningStatus(turnId) {
    if (!turnId) return;
    const status = Array.from(elements.timeline.querySelectorAll("[data-turn-status]"))
      .find((node) => node.dataset.turnStatus === String(turnId));
    status?.remove();
  }

  function commitRedoLive(live) {
    if (!live || live.operation !== "redo" || live.redoCommitted) return;
    live.redoCommitted = true;
    closeRevisionEditor();
    const stashKey = String(live.turnId || "");
    const previousStash = state.finishedTurnArticles.get(stashKey) || [];
    for (const entry of previousStash) {
      if (entry.kind === "final") entry.article?.remove();
    }
    const prefixSegments = previousStash.filter((entry) => entry.kind === "segment");
    if (prefixSegments.length) state.finishedTurnArticles.set(stashKey, prefixSegments);
    else state.finishedTurnArticles.delete(stashKey);
    for (const article of elements.timeline.querySelectorAll(".assistant-message")) {
      if (article.dataset.turnId === String(live.turnId || "") && article.dataset.segmentKind === "final") {
        article.remove();
      }
    }
    removeRunningStatus(live.turnId);
    if (live.inputId && live.editedContent != null) {
      const user = Array.from(elements.timeline.querySelectorAll(".user-message"))
        .find((article) => article.dataset.inputId === String(live.inputId));
      const paragraph = user?.querySelector(".user-bubble p");
      if (paragraph) paragraph.textContent = String(live.editedContent);
    }
    const turn = state.turns.find((item) => String(item?.id) === String(live.turnId));
    if (turn) {
      turn.status = "running";
      turn.assistant_content = "";
      turn.assistant_reasoning = null;
    }
    showTypingIndicator(live);
  }

  function createTypingIndicator() {
    const indicator = document.createElement("div");
    indicator.className = "typing-indicator";
    indicator.setAttribute("aria-hidden", "true");
    for (let index = 0; index < 3; index += 1) indicator.appendChild(document.createElement("i"));
    return indicator;
  }

  /* 发送后、第一个内容 part 到达前:气泡内三点弹跳等待动画 */
  function showTypingIndicator(live) {
    if (!live || live.ended || live.typing) return;
    ensureLiveArticle(live);
    if (live.blocks.childElementCount > 0) return;
    const indicator = createTypingIndicator();
    live.blocks.appendChild(indicator);
    live.typing = indicator;
    contentAdded();
  }

  function promoteTypingIndicator(live) {
    if (!live || live.ended) return;
    ensureLiveArticle(live);
    let indicator = live.typing;
    if (indicator?.classList.contains("is-streaming")) return;
    const start = indicator?.getBoundingClientRect() || null;
    if (!indicator) {
      indicator = createTypingIndicator();
      live.typing = indicator;
    }
    live.streamRail.hidden = false;
    live.streamRail.appendChild(indicator);
    indicator.classList.add("is-streaming");
    live.article.classList.add("is-streaming");
    if (start && !window.matchMedia("(prefers-reduced-motion: reduce)").matches) {
      const finish = indicator.getBoundingClientRect();
      live.typingAnimation?.cancel();
      live.typingAnimation = indicator.animate([
        { transform: `translate(${visualPixelsToLayout(start.left - finish.left)}px, ${visualPixelsToLayout(start.top - finish.top)}px)` },
        { transform: "translate(0, 0)" }
      ], { duration: 260, easing: "cubic-bezier(0.2, 0, 0, 1)" });
    }
    contentAdded();
  }

  function clearTypingIndicator(live, { waitingOnly = false } = {}) {
    if (!live?.typing) return;
    if (waitingOnly && live.typing.classList.contains("is-streaming")) return;
    live.typingAnimation?.cancel();
    live.typingAnimation = null;
    live.typing.remove();
    live.typing = null;
    if (live.streamRail) live.streamRail.hidden = true;
    live.article?.classList.remove("is-streaming");
  }

  /* 完成态保时序:live 渲染出的 article 按 turn 存档,重渲染时原样复用 */
  function stashLiveArticle(live, kind) {
    if (!live?.article) return;
    clearTypingIndicator(live);
    if (!live.turnId) return;
    if (!live.blocks || live.blocks.childElementCount === 0) return;
    live.article.classList.remove("live-assistant");
    live.article.dataset.segmentKind = kind;
    const key = String(live.turnId);
    const list = state.finishedTurnArticles.get(key) || [];
    list.push({ kind, article: live.article });
    state.finishedTurnArticles.set(key, list);
  }

  function updateLiveStopButton(live) {
    if (!live.stopButton) return;
    live.stopButton.disabled = live.ended || live.cancellationRequested;
    live.stopButton.title = live.cancellationRequested ? "正在停止" : "停止本条回复";
    live.stopButton.setAttribute("aria-label", live.stopButton.title);
  }

  function removeLiveStopButton(live) {
    if (!live.stopButton) return;
    live.stopButton.remove();
    live.stopButton = null;
    elements.liveStopRail.hidden = elements.liveStopRail.childElementCount === 0;
  }

  async function cancelLiveRun(live) {
    if (!live || live.ended || live.cancellationRequested) return;
    live.cancellationRequested = true;
    updateLiveStopButton(live);
    if (live.headerStatus) live.headerStatus.textContent = "正在停止";
    try {
      await apiRequest(`/api/runs/${encodeURIComponent(live.runId)}/cancel`, { method: "POST" });
    } catch (error) {
      live.cancellationRequested = false;
      updateLiveStopButton(live);
      if (live.headerStatus && !live.ended) live.headerStatus.textContent = "正在回复";
      showToast(error.message || "停止失败", "error");
      if ((error.status === 404 || error.status === 409) && state.viewSessionId) {
        await loadSessionView(state.viewSessionId, { quiet: true });
      }
    }
  }

  // 普通 Markdown 随内容收缩；只有需要稳定横向空间的结构撑满消息列。
  const WIDE_BLOCK_SELECTOR = ".markdown-body pre, .markdown-table-scroll, .conversation-media, .context-operation, img, .tool-card:not(.collapsed), .tool-live-progress:not([hidden])";
  function syncBubbleWidth(article) {
    if (!article) return;
    const content = article.querySelector(".assistant-content");
    if (!content) return;
    content.classList.toggle("is-slim", !content.querySelector(WIDE_BLOCK_SELECTOR));
  }

  function ensureLiveArticle(live) {
    if (live.article) return live.article;
    ensureTimelineVisible();
    ensureLiveUser(live, live.userText);
    removeRunningStatus(live.turnId);
    const article = document.createElement("article");
    article.className = "message assistant-message live-assistant";
    article.dataset.role = "assistant";
    article.dataset.runId = live.runId;
    if (live.turnId) article.dataset.turnId = String(live.turnId);
    const header = document.createElement("header");
    header.className = "assistant-label";
    const avatar = document.createElement("img");
    avatar.alt = "";
    avatar.setAttribute("aria-hidden", "true");
    setPersonaAvatar(avatar);
    const identity = document.createElement("div");
    const name = document.createElement("strong");
    name.textContent = state.persona.name;
    const status = document.createElement("span");
    status.className = "live-indicator";
    // 直播状态由三点弹跳/思考签表达,header 不再写「正在回复」;完成后写「刚刚」等
    status.textContent = "";
    identity.append(name, status);
    // Each running reply owns a compact stop control in its bubble corner.
    const stop = document.createElement("button");
    stop.type = "button";
    stop.className = "live-stop-button";
    stop.dataset.runId = live.runId;
    stop.appendChild(makeIconSlot("stop-square"));
    stop.addEventListener("click", () => cancelLiveRun(live));
    header.append(avatar, identity);
    for (const existing of elements.liveStopRail.querySelectorAll(".live-stop-button")) {
      if (existing.dataset.runId === live.runId) existing.remove();
    }
    elements.liveStopRail.appendChild(stop);
    elements.liveStopRail.hidden = false;
    const assistantContent = document.createElement("div");
    assistantContent.className = "assistant-content is-slim";
    const blocks = document.createElement("div");
    blocks.className = "assistant-blocks";
    assistantContent.appendChild(blocks);
    const bubble = document.createElement("div");
    bubble.className = "assistant-bubble";
    bubble.appendChild(assistantContent);
    const meta = document.createElement("div");
    meta.className = "assistant-meta";
    const endpoint = document.createElement("span");
    endpoint.className = "assistant-endpoint";
    endpoint.hidden = true;
    const metaText = document.createElement("span");
    metaText.textContent = "";
    const spacer = document.createElement("span");
    spacer.className = "meta-spacer";
    const copy = makeCopyButton(() => live.assistantText, "复制回复");
    copy.hidden = true;
    meta.append(endpoint, metaText, spacer, copy);
    const streamRail = document.createElement("div");
    streamRail.className = "assistant-stream-rail";
    streamRail.hidden = true;
    article.append(header, bubble, meta, streamRail);
    elements.timeline.appendChild(article);
    live.article = article;
    live.blocks = blocks;
    live.headerStatus = status;
    live.stopButton = stop;
    live.meta = metaText;
    live.endpoint = endpoint;
    live.copyButton = copy;
    live.streamRail = streamRail;
    updateLiveStopButton(live);
    contentAdded();
    return article;
  }

  function breakLiveText(live) {
    live.currentText = null;
  }

  function scheduleMarkdownRender(block) {
    if (block.renderFrame) return;
    block.renderFrame = window.requestAnimationFrame(() => {
      block.renderFrame = null;
      renderMarkdown(block.element, block.raw);
      contentAdded();
    });
  }

  function appendAssistantDelta(live, delta) {
    const text = String(delta || "");
    if (!text) return;
    ensureLiveArticle(live);
    const startsText = !live.currentText;
    if (!live.currentText) {
      finalizeLiveReasoning(live);
      const element = document.createElement("div");
      element.className = "markdown-body live-text-block";
      const block = { element, raw: "", renderFrame: null };
      live.blocks.appendChild(element);
      syncBubbleWidth(live.article);
      live.currentText = block;
      live.contextOperation = null;
      if (live.assistantText && !/\s$/.test(live.assistantText)) live.assistantText += "\n\n";
    }
    live.currentText.raw += text;
    live.assistantText += text;
    live.copyButton.hidden = !live.assistantText.trim();
    if (startsText) {
      renderMarkdown(live.currentText.element, live.currentText.raw);
      promoteTypingIndicator(live);
    } else {
      scheduleMarkdownRender(live.currentText);
    }
    contentAdded();
  }

  function resetSupersededGeneration(live) {
    if (live.currentText?.renderFrame) window.cancelAnimationFrame(live.currentText.renderFrame);
    live.currentText?.element?.remove();
    live.currentText = null;
    for (const reasoning of live.reasoningParts || []) reasoning.element?.remove();
    if (live.reasoningTimer) window.clearInterval(live.reasoningTimer);
    live.reasoningTimer = null;
    live.reasoning = null;
    live.reasoningParts = [];
    live.reasoningStarted = false;
    live.reasoningTitle = "";
    live.reasoningClockStart = null;
    live.assistantText = "";
    live.assistantReasoning = "";
    if (live.copyButton) live.copyButton.hidden = true;
    clearTypingIndicator(live);
    showTypingIndicator(live);
  }

  function ensureLiveReasoning(live) {
    ensureLiveArticle(live);
    clearTypingIndicator(live, { waitingOnly: true });
    if (live.reasoning) return live.reasoning;
    breakLiveText(live);
    live.contextOperation = null;
    const reasoning = createReasoningBlock("", "正在思考", true);
    // 计时从 reasoning.start 事件算起,而不是签出现的时刻(签是惰性创建的)
    if (live.reasoningClockStart != null) reasoning.startedAt = live.reasoningClockStart;
    reasoning.pendingTitle = normalizeReasoningTitle(live.reasoningTitle);
    if (!reasoningHidden()) live.blocks.appendChild(reasoning.element);
    live.reasoning = reasoning;
    live.reasoningParts.push(reasoning);
    if (live.reasoningTimer) window.clearInterval(live.reasoningTimer);
    const updateProgress = () => {
      if (!reasoning.liveStatus || reasoning.startedAt == null) return;
      const elapsed = Math.max(0, Math.floor((performance.now() - reasoning.startedAt) / 1000));
      reasoning.liveStatus.textContent = `${elapsed}s`;
    };
    updateProgress();
    live.reasoningTimer = window.setInterval(updateProgress, 1000);
    return reasoning;
  }

  function collectLiveReasoning(live) {
    return (live.reasoningParts || [])
      .map((part) => String(part.raw || "").trim())
      .filter(Boolean)
      .join("\n\n");
  }

  function finalizeLiveReasoning(live) {
    const reasoning = live.reasoning;
    if (!reasoning) return;
    if (live.reasoningTimer) {
      window.clearInterval(live.reasoningTimer);
      live.reasoningTimer = null;
    }
    const parsed = splitReasoningText(reasoning.raw);
    const title = "已思考";
    reasoning.raw = parsed.body;
    reasoning.finished = true;
    if (!reasoning.raw.trim() && title === "已思考") {
      reasoning.element.remove();
    } else {
      reasoning.element.classList.remove("is-live");
      reasoning.title.textContent = title;
      reasoning.body.textContent = reasoning.raw;
      if (reasoning.progress) reasoning.progress.remove();
      if (reasoning.liveStatus) {
        if (reasoning.startedAt != null) {
          reasoning.liveStatus.textContent = `${((performance.now() - reasoning.startedAt) / 1000).toFixed(1)}s`;
        } else {
          reasoning.liveStatus.remove();
        }
      }
    }
    live.reasoning = null;
    live.reasoningTitle = "";
    live.reasoningStarted = false;
    live.reasoningClockStart = null;
    live.assistantReasoning = collectLiveReasoning(live);
  }

  function handleReasoningEvent(name, live, data) {
    if (name === "reasoning.start" || name === "reasoning.part_start") {
      // 惰性创建:只记状态,签等第一段真实思考文本(reasoning.delta)到达才出现,
      // 避免不输出思考的模型挂着空的「正在思考」签和空面板
      finalizeLiveReasoning(live);
      live.reasoningStarted = true;
      live.reasoningClockStart = performance.now();
      breakLiveText(live);
      return;
    }
    if (name === "reasoning.reset") {
      if (live.reasoning) {
        live.reasoning.raw = "";
        live.reasoning.body.textContent = "";
        live.reasoning.pendingTitle = "";
      }
      return;
    }
    if (name === "reasoning.title") {
      live.reasoningTitle = String(data?.title || "").trim();
      // 只更新已存在的签;没有思考文本就不为标题单独建签
      if (live.reasoning) live.reasoning.pendingTitle = normalizeReasoningTitle(live.reasoningTitle);
      return;
    }
    if (name === "reasoning.delta") {
      const delta = String(data?.delta || "");
      if (!delta) return;
      if (!live.reasoning && !delta.trim()) return;
      const reasoning = ensureLiveReasoning(live);
      reasoning.raw += delta;
      reasoning.body.textContent = reasoning.raw;
      live.assistantReasoning = collectLiveReasoning(live);
      contentAdded();
      return;
    }
    if (name === "reasoning.part_end") {
      finalizeLiveReasoning(live);
    }
  }

  function prettyArguments(value) {
    if (value == null) return "";
    if (typeof value === "string") {
      const trimmed = value.trim();
      if (!trimmed) return "";
      try {
        return JSON.stringify(JSON.parse(trimmed), null, 2);
      } catch (_) {
        return value;
      }
    }
    try {
      return JSON.stringify(value, null, 2);
    } catch (_) {
      return String(value);
    }
  }

  function parsedToolArguments(value) {
    if (value && typeof value === "object" && !Array.isArray(value)) return value;
    if (typeof value !== "string" || !value.trim()) return {};
    try {
      const parsed = JSON.parse(value);
      return parsed && typeof parsed === "object" && !Array.isArray(parsed) ? parsed : {};
    } catch (_) {
      return {};
    }
  }

  function compactLine(value, limit = 92) {
    const line = String(value || "").replace(/\s+/g, " ").trim();
    if (line.length <= limit) return line;
    return `${line.slice(0, Math.max(1, limit - 1))}…`;
  }

  function compactPath(value) {
    const path = String(value || "").trim();
    if (!path) return "";
    return path.split(/[\\/]/).filter(Boolean).pop() || path;
  }

  function toolSubject(name, value) {
    const args = parsedToolArguments(value);
    const toolName = String(name || "");
    if (toolName === "run_command") {
      const line = compactLine(args.command || args.cmd);
      return args.background === true ? `[后台] ${line}` : line;
    }
    if (toolName === "read_file") {
      const path = compactPath(args.path);
      const offset = Number.isFinite(Number(args.offset)) && args.offset != null ? Number(args.offset) : null;
      const limit = Number.isFinite(Number(args.limit)) && args.limit != null ? Number(args.limit) : null;
      if (offset === null && limit === null) return path;
      const start = Math.max(offset ?? 1, 1);
      const page = limit !== null ? `L${start}-${start + limit - 1}` : `L${start}+`;
      return path ? `${path} (${page})` : page;
    }
    if (toolName === "apply_patch" || toolName === "apply_artifact_patch") {
      // 唯一编辑器:patchText 里抠出文件名当副标题,不然标签恒空。
      const text = String(args.patchText || args.patch_text || "");
      const files = [...text.matchAll(/^\*\*\* (?:Add|Update|Delete) File: (.+)$/gm)].map((m) => m[1].trim());
      if (files.length === 1) return compactPath(files[0]);
      if (files.length > 1) return `${compactPath(files[0])} 等 ${files.length} 个文件`;
      return "";
    }
    if (["read", "write", "edit", "print_image", "vision_analyze"].includes(toolName)) {
      return compactPath(args.filePath || args.file_path || args.path || args.image);
    }
    if (toolName === "grep") {
      const target = compactPath(args.path);
      return compactLine(`${args.pattern || ""}${target ? ` · ${target}` : ""}`);
    }
    if (toolName === "glob") return compactLine(`${args.pattern || ""}${args.path ? ` · ${compactPath(args.path)}` : ""}`);
    if (["webfetch", "web_fetch"].includes(toolName)) return compactLine(args.url);
    if (["web_search", "search_web", "search_web_images"].includes(toolName)) return compactLine(args.query || args.q);
    if (toolName === "generate_image") return compactLine(args.prompt);
    if (toolName === "task") return compactLine(args.description || args.prompt);
    if (toolName === "load_skill") return compactLine(args.name);
    const preferred = ["query", "command", "path", "filePath", "url", "name", "id", "target"];
    for (const key of preferred) {
      if (typeof args[key] === "string" && args[key].trim()) return compactLine(args[key]);
    }
    return "";
  }

  function formatToolDuration(milliseconds) {
    if (!Number.isFinite(milliseconds) || milliseconds < 0) return "";
    if (milliseconds < 1_000) return `${Math.max(1, Math.round(milliseconds))} ms`;
    if (milliseconds < 10_000) return `${(milliseconds / 1_000).toFixed(1)} s`;
    return `${Math.round(milliseconds / 1_000)} s`;
  }

  // 主题与工具显示名共享 ≥6 字符前缀时去重(如「Linux 游戏兼容性调查」+「Linux 游戏兼容性: xxx」)
  function dedupeToolSubject(title, subject) {
    const t = String(title || "").trim();
    const s = String(subject || "").trim();
    if (!t || !s) return s;
    let i = 0;
    while (i < t.length && i < s.length && t[i] === s[i]) i += 1;
    if (i < 6) return s;
    const rest = s.slice(i).replace(/^[\s:：·,，、-]+/, "");
    return rest || s;
  }

  function updateToolSummary(tool) {
    const details = [];
    const subject = dedupeToolSubject(tool.titleText, tool.subject);
    if (tool.commandPreview) {
      tool.commandPreview.textContent = tool.commandText || subject || "等待命令";
      tool.summary.textContent = tool.finishedAt == null
        ? ""
        : formatToolDuration(tool.finishedAt - tool.startedAt);
      return;
    }
    if (subject) details.push(subject);
    if (tool.imageCount) details.push(`${tool.imageCount} 张图片`);
    if (tool.finishedAt != null) details.push(formatToolDuration(tool.finishedAt - tool.startedAt));
    tool.summary.textContent = details.filter(Boolean).join(" · ") || (tool.finished ? "无输出" : "等待输出");
  }

  function scrollToolOutputToEnd(tool) {
    for (const detail of [tool.stdoutDetail, tool.stderrDetail, tool.resultDetail]) {
      if (!detail.wrapper.hidden) detail.content.scrollTop = detail.content.scrollHeight;
    }
  }

  function boundedAppend(current, addition) {
    const combined = `${current || ""}${addition || ""}`;
    if (combined.length <= MAX_TOOL_OUTPUT_CHARS) return combined;
    return `[较早输出已省略]\n${combined.slice(combined.length - MAX_TOOL_OUTPUT_CHARS)}`;
  }

  function createToolDetail(labelText, preformatted = false) {
    const wrapper = document.createElement("div");
    wrapper.className = "tool-detail";
    wrapper.hidden = true;
    const label = document.createElement("span");
    label.className = "tool-detail-label";
    label.textContent = labelText;
    const content = document.createElement(preformatted ? "pre" : "p");
    wrapper.append(label, content);
    return { wrapper, content, raw: "" };
  }

  function updateToolStatus(tool, status, iconName, statusClass = "") {
    tool.statusText.textContent = status;
    tool.statusIcon.replaceChildren(createIcon(iconName));
    tool.statusIcon.classList.toggle("is-spinning", iconName === "loader-circle");
    tool.card.classList.remove("is-success", "is-failure");
    if (statusClass) tool.card.classList.add(statusClass);
  }

  function renderCommandOutputPreview(tool) {
    const preview = tool.pendingOutputPreview;
    const panel = tool.commandOutputPreview;
    if (!panel || !preview || !Array.isArray(preview.lines)) return;
    const wasFollowing = panel.hidden || panel.scrollHeight - panel.scrollTop - panel.clientHeight <= 2;
    const previousScrollTop = panel.scrollTop;
    const children = [];
    if (preview.omitted) {
      const omitted = document.createElement("span");
      omitted.className = "tool-command-output-omitted";
      omitted.textContent = "⋮ 已省略较早输出";
      children.push(omitted);
    }
    for (const line of preview.lines) {
      const row = document.createElement("span");
      row.className = `tool-command-output-line${line?.stream === "stderr" ? " is-stderr" : ""}`;
      row.textContent = String(line?.text || "");
      children.push(row);
    }
    panel.replaceChildren(...children);
    panel.hidden = children.length === 0;
    if (!panel.hidden) panel.scrollTop = wasFollowing ? panel.scrollHeight : previousScrollTop;
  }

  function scheduleCommandOutputPreview(tool, preview) {
    if (!tool?.commandOutputPreview || !preview || typeof preview !== "object") return;
    tool.pendingOutputPreview = preview;
    if (tool.outputRenderFrame) return;
    tool.outputRenderFrame = window.requestAnimationFrame(() => {
      tool.outputRenderFrame = null;
      renderCommandOutputPreview(tool);
      contentAdded();
    });
  }

  // 工具家族图标(验收清单):终端=$、网络=地球仪、编辑=笔、记忆=大脑……
  // 未列家族回落扳手。全部 lucide 线稿,无 emoji。
  function toolIconName(name) {
    const n = String(name || "");
    if (["run_command", "job_status", "job_stop"].includes(n)) return "terminal";
    if (["web_search", "web_fetch", "search_web", "webfetch"].includes(n)) return "globe";
    if (n === "search_web_images") return "image-search";
    if (n === "apply_patch" || n === "apply_artifact_patch") return "square-pen";
    if (["recall_memories", "recall_past_events", "remember_fact", "search_evicted_context"].includes(n)) return "brain";
    if (["create_goal", "get_goal", "update_goal"].includes(n)) return "target";
    if (n === "todowrite" || n === "todoupdate") return "list-todo";
    if (n === "task" || n === "deep_research") return "bot";
    if (n.includes("knowledge_base")) return "book-open";
    if (n === "ask_question") return "circle-help";
    if (n === "generate_image") return "paintbrush";
    if (["analyze_image", "vision_analyze", "print_image"].includes(n)) return "image";
    if (n.includes("meme")) return "smile";
    if (n.includes("alarm")) return "alarm-clock";
    if (n === "read_clipboard") return "clipboard";
    if (n === "get_weather") return "cloud-sun";
    if (["calculator", "scientific_calculator", "calculate_hash", "get_exchange_rate", "decode_encoded_text"].includes(n)) return "calculator";
    if (n === "read_file") return "file-text";
    if (n === "glob" || n === "grep") return "search";
    if (n === "trash_path") return "trash-2";
    if (n === "load_tools") return "package";
    if (n.includes("skill")) return "puzzle";
    if (n.startsWith("aur_") || n.startsWith("archlinux") || n.startsWith("archwiki") || n === "install_aur_package") return "arch";
    if (n.startsWith("online_man")) return "package";
    if (n === "usage_query") return "chart-column";
    if (["draw_tarot_card", "draw_zhouyi_hexagram", "draw_fortune_lot"].includes(n)) return "sparkles";
    if (["create_artifact", "read_artifact", "present_artifact"].includes(n)) return "file-text";
    return "wrench";
  }

  function createTool(live, data) {
    ensureLiveArticle(live);
    clearTypingIndicator(live, { waitingOnly: true });
    breakLiveText(live);
    finalizeLiveReasoning(live);
    live.contextOperation = null;
    const toolId = String(data?.tool_id || `${live.runId}_tool_unknown_${live.tools.size + 1}`);
    if (live.tools.has(toolId)) return live.tools.get(toolId);
    const card = document.createElement("section");
    card.className = state.toolExpanded ? "tool-card" : "tool-card collapsed";
    card.dataset.toolId = toolId;
    const isCommand = String(data?.name || "") === "run_command";
    if (isCommand) card.classList.add("is-command");
    const isTask = String(data?.name || "") === "task" || /^task[:：]/i.test(String(data?.display_name || ""));
    if (isTask) card.classList.add("is-task");
    const subjectText = toolSubject(data?.name, data?.arguments);
    const commandArguments = isCommand ? parsedToolArguments(data?.arguments) : null;
    const commandText = isCommand ? String(commandArguments?.command || commandArguments?.cmd || "").trim() : "";
    const head = document.createElement("button");
    head.className = "tool-head";
    head.type = "button";
    head.setAttribute("aria-expanded", String(Boolean(state.toolExpanded)));
    const icon = document.createElement("span");
    icon.className = "tool-icon";
    const toolName = String(data?.name || "");
    icon.appendChild(makeIconSlot(toolIconName(toolName)));
    const title = document.createElement("span");
    title.className = "tool-title";
    const displayName = document.createElement("strong");
    displayName.textContent = String(data?.display_name || data?.name || "工具");
    const realName = document.createElement("small");
    realName.className = "tool-technical-name";
    realName.textContent = String(data?.name || "");
    const summary = document.createElement("small");
    summary.className = "tool-summary";
    title.append(displayName, realName, summary);
    const status = document.createElement("span");
    status.className = "tool-status";
    const statusIcon = makeIconSlot("loader-circle", "is-spinning");
    const statusText = document.createElement("span");
    statusText.textContent = "运行中";
    status.append(statusIcon, statusText);
    const chevron = makeIconSlot("chevron-down", "tool-chevron");
    head.append(icon, title, status, chevron);
    let commandPreview = null;
    let commandOutputPreview = null;
    if (isCommand) {
      commandPreview = document.createElement("pre");
      commandPreview.className = "tool-command-preview";
      commandPreview.textContent = commandText || subjectText || "等待命令";
      commandOutputPreview = document.createElement("div");
      commandOutputPreview.className = "tool-command-output-preview";
      commandOutputPreview.setAttribute("aria-label", "最近命令输出");
      commandOutputPreview.style.setProperty("--command-output-lines", String(COMMAND_OUTPUT_PREVIEW_ROWS));
      commandOutputPreview.hidden = true;
    }
    const body = document.createElement("div");
    body.className = "tool-body";
    const argumentsDetail = createToolDetail("参数", true);
    const progressDetail = createToolDetail("进度");
    const stdoutDetail = createToolDetail("命令输出", true);
    const stderrDetail = createToolDetail("错误输出", true);
    stderrDetail.wrapper.classList.add("is-stderr");
    const resultDetail = createToolDetail("结果", true);
    const argumentText = prettyArguments(data?.arguments);
    if (argumentText) {
      argumentsDetail.raw = argumentText;
      argumentsDetail.content.textContent = argumentText;
      argumentsDetail.wrapper.hidden = false;
    }
    body.append(argumentsDetail.wrapper, progressDetail.wrapper, stdoutDetail.wrapper, stderrDetail.wrapper, resultDetail.wrapper);
    // 子代理签:标题行下方的实时进度面板,收起态也可见,tool.progress 原地刷新
    let liveProgress = null;
    if (isTask) {
      liveProgress = document.createElement("div");
      liveProgress.className = "tool-live-progress";
      liveProgress.textContent = subjectText || "正在启动子代理…";
      card.append(head, liveProgress, body);
    } else {
      card.append(head);
      if (commandPreview) card.appendChild(commandPreview);
      if (commandOutputPreview) card.appendChild(commandOutputPreview);
      card.appendChild(body);
    }
    const tool = {
      id: toolId,
      name: String(data?.name || ""),
      card,
      head,
      body,
      status,
      statusIcon,
      statusText,
      summary,
      commandPreview,
      commandOutputPreview,
      commandText,
      artifactPreview: null,
      pendingOutputPreview: null,
      outputRenderFrame: null,
      argumentsDetail,
      progressDetail,
      stdoutDetail,
      stderrDetail,
      resultDetail,
      isTask,
      liveProgress,
      titleText: String(data?.display_name || data?.name || "工具"),
      subject: subjectText,
      startedAt: performance.now(),
      finishedAt: null,
      imageCount: 0,
      finished: false,
      collapseTimer: null
    };
    head.addEventListener("click", () => {
      const collapsed = card.classList.toggle("collapsed");
      head.setAttribute("aria-expanded", String(!collapsed));
      syncBubbleWidth(live.article);
      if (!collapsed) {
        window.requestAnimationFrame(() => {
          scrollToolOutputToEnd(tool);
          contentAdded();
        });
      }
    });
    updateToolSummary(tool);
    live.tools.set(toolId, tool);
    live.blocks.appendChild(card);
    syncBubbleWidth(live.article);
    contentAdded();
    return tool;
  }

  function ensureTool(live, data) {
    const toolId = String(data?.tool_id || "");
    return (toolId && live.tools.get(toolId)) || createTool(live, data);
  }

  // The backend sends the phase text; the local map is only a fallback for a
  // daemon older than this asset.
  function preparingToolLabel(name, phase) {
    if (phase) return String(phase);
    if (name === "apply_patch" || name === "apply_artifact_patch") return "准备编辑";
    if (name === "run_command") return "准备执行";
    if (name === "ask_question") return "准备问题";
    return "准备工具";
  }

  function clearPreparingTool(live) {
    if (!live?.preparingTool) return;
    live.preparingTool.remove();
    live.preparingTool = null;
    contentAdded();
  }

  function handleToolPreparing(live, data) {
    const name = String(data?.tool_name || "");
    if (!name) return;
    ensureLiveArticle(live);
    clearTypingIndicator(live, { waitingOnly: true });
    finalizeLiveReasoning(live);
    if (live.preparingTool?.dataset.toolName === name) return;
    clearPreparingTool(live);
    const tag = document.createElement("div");
    tag.className = "tool-preparing-tag";
    tag.dataset.toolName = name;
    const label = document.createElement("span");
    label.textContent = preparingToolLabel(name, data?.phase);
    tag.append(makeIconSlot("loader-circle", "is-spinning"), label);
    live.blocks.appendChild(tag);
    live.preparingTool = tag;
    syncBubbleWidth(live.article);
    contentAdded();
  }

  function handleToolEvent(name, live, data) {
    if (name === "tool.preparing") {
      handleToolPreparing(live, data);
      return;
    }
    if (name === "tool.started") {
      clearPreparingTool(live);
      createTool(live, data);
      return;
    }
    const tool = ensureTool(live, data);
    if (name === "tool.image") {
      const asset = data?.asset && typeof data.asset === "object" ? data.asset : null;
      if (asset && safeAssetUrl(asset.url)) {
        const assetId = String(asset.id || asset.url);
        if (!live.assets.some((item) => String(item?.id || item?.url) === assetId)) {
          ensureLiveArticle(live);
          clearTypingIndicator(live, { waitingOnly: true });
          breakLiveText(live);
          finalizeLiveReasoning(live);
          live.contextOperation = null;
          live.assets.push(asset);
          live.blocks.appendChild(createConversationMedia(asset, { eager: true }));
          registerArtifact({ ...asset, name: String(asset.alt || "生成的图片"), kind: "image" }, { autoOpen: true });
          syncBubbleWidth(live.article);
          tool.imageCount += 1;
        }
      } else if (data?.error) {
        const message = String(data.error);
        tool.progressDetail.raw = message;
        tool.progressDetail.content.textContent = message;
        tool.progressDetail.wrapper.hidden = Boolean(tool.liveProgress);
        if (tool.liveProgress) {
          tool.liveProgress.textContent = message;
          tool.liveProgress.hidden = false;
        }
      }
      updateToolSummary(tool);
    } else if (name === "tool.artifact") {
      const artifact = normalizeArtifact(data?.artifact, "file");
      if (artifact) {
        registerArtifact(artifact, { autoOpen: true });
        if (!live.artifacts) live.artifacts = [];
        const index = live.artifacts.findIndex((item) => String(item?.id) === artifact.id);
        if (index >= 0) live.artifacts[index] = artifact;
        else live.artifacts.push(artifact);
        if (!tool.artifactPreview) {
          tool.artifactPreview = document.createElement("button");
          tool.artifactPreview.type = "button";
          tool.artifactPreview.className = "tool-artifact-preview";
          tool.card.insertBefore(tool.artifactPreview, tool.body);
          tool.artifactPreview.addEventListener("click", () => {
            const current = state.artifacts.find((item) => item.id === tool.artifactPreview.dataset.artifactId);
            if (!current) return;
            state.selectedArtifactId = current.id;
            setArtifactWorkspaceOpen(true);
          });
        }
        tool.artifactPreview.dataset.artifactId = artifact.id;
        const artifactLabel = document.createElement("span");
        artifactLabel.textContent = artifact.name;
        tool.artifactPreview.replaceChildren(
          makeIconSlot(artifactIconName(artifact)),
          artifactLabel,
          makeIconSlot("panel-right")
        );
        tool.subject = artifact.name;
      } else if (data?.error) {
        tool.progressDetail.raw = String(data.error);
        tool.progressDetail.content.textContent = tool.progressDetail.raw;
        tool.progressDetail.wrapper.hidden = false;
      }
      updateToolSummary(tool);
    } else if (name === "tool.progress") {
      let message = String(data?.message || "");
      if (message.startsWith("__tool_phase__")) {
        message = message.slice("__tool_phase__".length).replace(/^~\s*/, "").trim();
      } else if (message.startsWith("__subagent_stats__")) {
        message = message.slice("__subagent_stats__".length).trim();
      } else if (message.startsWith("__subagent_detach__")) {
        message = message.slice("__subagent_detach__".length).trim();
      }
      // 任何持续汇报进度的工具(插件子代理如深度研究/兼容性调查)都惰性获得实时进度面板,
      // 不再仅限内置 task 工具
      if (!tool.liveProgress && !tool.finished && message) {
        tool.liveProgress = document.createElement("div");
        tool.liveProgress.className = "tool-live-progress";
        tool.card.insertBefore(tool.liveProgress, tool.body);
      }
      tool.progressDetail.raw = message;
      tool.progressDetail.content.textContent = message;
      tool.progressDetail.wrapper.hidden = !message || Boolean(tool.liveProgress);
      if (tool.liveProgress && message) {
        tool.liveProgress.textContent = message;
        tool.liveProgress.hidden = false;
        syncBubbleWidth(live.article);
      }
      if (!tool.subject && message) tool.subject = compactLine(message);
      updateToolStatus(tool, "运行中", "loader-circle");
      updateToolSummary(tool);
    } else if (name === "tool.output") {
      const detail = data?.stream === "stderr" ? tool.stderrDetail : tool.stdoutDetail;
      detail.raw = boundedAppend(detail.raw, String(data?.output || ""));
      detail.content.textContent = detail.raw;
      detail.wrapper.hidden = !detail.raw;
      if (!tool.card.classList.contains("collapsed")) detail.content.scrollTop = detail.content.scrollHeight;
      scheduleCommandOutputPreview(tool, data?.preview);
      updateToolSummary(tool);
    } else if (name === "tool.finished") {
      tool.finished = true;
      tool.finishedAt = performance.now();
      const output = String(data?.output || "");
      tool.resultDetail.raw = output.length > MAX_TOOL_OUTPUT_CHARS ? `[较早输出已省略]\n${output.slice(-MAX_TOOL_OUTPUT_CHARS)}` : output;
      tool.resultDetail.content.textContent = tool.resultDetail.raw;
      tool.resultDetail.wrapper.hidden = !tool.resultDetail.raw;
      const ok = Boolean(data?.ok);
      scheduleCommandOutputPreview(tool, data?.preview);
      updateToolStatus(tool, ok ? "完成" : "失败", ok ? "check" : "circle-alert", ok ? "is-success" : "is-failure");
      updateToolSummary(tool);
      if (tool.liveProgress) {
        if (ok) tool.liveProgress.hidden = true;
        else tool.liveProgress.classList.add("is-error");
        tool.progressDetail.wrapper.hidden = !tool.progressDetail.raw;
        syncBubbleWidth(live.article);
      }
      if (!state.toolExpanded) {
        tool.card.classList.add("collapsed");
        tool.head.setAttribute("aria-expanded", "false");
      }
    }
    contentAdded();
  }

  function questionHasAnswer(questionState, index = questionState.pageIndex) {
    const control = questionState.controls[index];
    if (!control) return false;
    return control.options.some((option) => option.input.checked)
      || Boolean(control.custom?.toggle.checked && control.custom.textarea.value.trim());
  }

  function updateQuestionNavigation(questionState) {
    if (!questionState?.questions?.length) return;
    const lastIndex = questionState.questions.length - 1;
    const atLastPage = questionState.pageIndex === lastIndex;
    const answered = questionHasAnswer(questionState);
    const canInteract = questionState.pending && !questionState.submitting && !questionState.closing;

    questionState.previous.disabled = !canInteract || questionState.pageIndex === 0;
    questionState.next.hidden = atLastPage;
    questionState.next.disabled = !canInteract || !answered;
    questionState.next.classList.toggle("is-ready", canInteract && answered && !atLastPage);
    questionState.submit.hidden = !atLastPage;
    questionState.submit.disabled = !canInteract || !answered;
    questionState.submit.classList.toggle("is-ready", canInteract && answered && atLastPage);
    questionState.close.disabled = !canInteract;

    questionState.controls.forEach((control, index) => {
      const custom = control.custom;
      if (!custom?.next) return;
      const customAnswered = Boolean(custom.toggle.checked && custom.textarea.value.trim());
      const show = canInteract && customAnswered;
      custom.next.hidden = !show;
      custom.next.disabled = !show;
      custom.next.classList.toggle("is-ready", show);
      custom.next.replaceChildren(makeIconSlot(index === lastIndex ? "check" : "chevron-right"));
      custom.next.title = index === lastIndex ? "提交回答" : "下一题";
      custom.next.setAttribute("aria-label", custom.next.title);
    });
  }

  function updateQuestionOptionClasses(questionState) {
    for (const control of questionState.controls) {
      for (const option of control.options) option.label.classList.toggle("selected", option.input.checked);
      if (control.custom) control.custom.wrapper.classList.toggle("selected", control.custom.toggle.checked);
    }
    updateQuestionNavigation(questionState);
  }

  function updateQuestionDock() {
    elements.questionDock.hidden = elements.questionDock.childElementCount === 0;
    elements.composerDock.classList.toggle("has-pending-question", !elements.questionDock.hidden);
    window.requestAnimationFrame(updateJumpButtonOffset);
  }

  function clearQuestionDock() {
    elements.questionDock.replaceChildren();
    updateQuestionDock();
  }

  function moveQuestionToTimeline(questionState) {
    if (questionState.card.parentElement !== elements.questionDock) return;
    if (questionState.timelineParent?.isConnected) questionState.timelineParent.appendChild(questionState.card);
    else questionState.card.remove();
    updateQuestionDock();
  }

  function removeQuestionFromDock(questionState) {
    if (questionState.card.parentElement === elements.questionDock) questionState.card.remove();
    updateQuestionDock();
  }

  function setQuestionPage(questionState, index, { focus = false } = {}) {
    if (!questionState?.pages?.length) return;
    if (questionState.autoAdvanceTimer) {
      window.clearTimeout(questionState.autoAdvanceTimer);
      questionState.autoAdvanceTimer = null;
    }
    const lastIndex = questionState.pages.length - 1;
    const nextIndex = Math.max(0, Math.min(lastIndex, Number(index) || 0));
    questionState.pageIndex = nextIndex;
    questionState.pages.forEach((page, pageIndex) => {
      page.hidden = pageIndex !== nextIndex;
    });
    const question = questionState.questions[nextIndex] || {};
    questionState.prompt.textContent = String(question.question || question.header || `问题 ${nextIndex + 1}`);
    questionState.position.textContent = `${nextIndex + 1} of ${questionState.pages.length}`;
    updateQuestionNavigation(questionState);
    elements.questionDock.scrollTop = 0;
    window.requestAnimationFrame(() => {
      updateJumpButtonOffset();
      if (focus) questionState.pages[nextIndex].querySelector("input:not(:disabled), textarea:not(:disabled)")?.focus();
    });
  }

  function advanceQuestion(questionState) {
    if (!questionState?.pending || questionState.submitting || !questionHasAnswer(questionState)) return;
    if (questionState.pageIndex >= questionState.pages.length - 1) {
      submitQuestion(questionState);
      return;
    }
    setQuestionPage(questionState, questionState.pageIndex + 1, { focus: true });
  }

  function selectedQuestionAnswers(questionState) {
    const answers = [];
    for (let index = 0; index < questionState.controls.length; index += 1) {
      const control = questionState.controls[index];
      const selected = control.options.filter((option) => option.input.checked).map((option) => option.value);
      if (control.custom?.toggle.checked) {
        const custom = control.custom.textarea.value.trim();
        if (!custom) throw new Error(`请填写第 ${index + 1} 项的自定义回答`);
        if (countCharacters(custom) > MAX_CUSTOM_ANSWER_CHARS) throw new Error(`第 ${index + 1} 项的自定义回答不能超过 4,000 个字符`);
        if (/[\u0000-\u001f\u007f-\u009f]/.test(custom)) throw new Error(`第 ${index + 1} 项的自定义回答不能包含控制字符或换行`);
        if (selected.includes(custom)) throw new Error(`第 ${index + 1} 项包含重复回答`);
        selected.push(custom);
      }
      if (selected.length === 0) throw new Error(`请回答第 ${index + 1} 项`);
      if (!control.multiple && selected.length !== 1) throw new Error(`第 ${index + 1} 项只能选择一个回答`);
      answers.push(selected);
    }
    return answers;
  }

  function setQuestionControlsDisabled(questionState, disabled) {
    questionState.form.querySelectorAll("input, textarea, button").forEach((control) => {
      control.disabled = disabled;
    });
  }

  function renderQuestionAnswerSummary(questionState, answers) {
    questionState.summary.replaceChildren();
    const normalized = Array.isArray(answers) ? answers : [];
    questionState.questions.forEach((question, index) => {
      const row = document.createElement("div");
      const term = document.createElement("dt");
      term.textContent = String(question?.question || question?.header || `问题 ${index + 1}`);
      const value = document.createElement("dd");
      value.textContent = (Array.isArray(normalized[index]) ? normalized[index] : []).map(String).join("、") || "未记录";
      row.append(term, value);
      questionState.summary.appendChild(row);
    });
    questionState.summary.hidden = false;
  }

  function markQuestionAnswered(questionState, answers) {
    if (!questionState || !questionState.pending) return;
    if (questionState.autoAdvanceTimer) window.clearTimeout(questionState.autoAdvanceTimer);
    questionState.autoAdvanceTimer = null;
    questionState.pending = false;
    questionState.submitting = false;
    questionState.closing = false;
    questionState.restoreFocusOnClose = false;
    questionState.answers = answers;
    questionState.card.classList.remove("is-error");
    questionState.card.classList.add("is-answered");
    questionState.card.removeAttribute("aria-busy");
    questionState.header.hidden = false;
    questionState.card.removeAttribute("aria-label");
    questionState.card.setAttribute("aria-labelledby", questionState.titleId);
    questionState.status.textContent = "已回答";
    questionState.icon.replaceChildren(makeIconSlot("check"));
    questionState.error.hidden = true;
    setQuestionControlsDisabled(questionState, true);
    renderQuestionAnswerSummary(questionState, answers);
    moveQuestionToTimeline(questionState);
    updateControlState();
    contentAdded();
  }

  function markQuestionClosed(questionState) {
    if (!questionState?.pending) return;
    const restoreFocus = questionState.restoreFocusOnClose || questionState.card.contains(document.activeElement);
    questionState.restoreFocusOnClose = false;
    if (questionState.autoAdvanceTimer) window.clearTimeout(questionState.autoAdvanceTimer);
    questionState.autoAdvanceTimer = null;
    questionState.pending = false;
    questionState.submitting = false;
    questionState.closing = false;
    questionState.card.removeAttribute("aria-busy");
    setQuestionControlsDisabled(questionState, true);
    removeQuestionFromDock(questionState);
    updateControlState();
    showToast("回答界面已关闭");
    if (restoreFocus) window.requestAnimationFrame(focusComposerIfDesktop);
    contentAdded();
  }

  async function closeQuestion(questionState) {
    if (!questionState?.pending || questionState.submitting || questionState.closing) return;
    questionState.restoreFocusOnClose = questionState.card.contains(document.activeElement);
    questionState.closing = true;
    questionState.error.hidden = true;
    questionState.card.classList.remove("is-error");
    questionState.card.setAttribute("aria-busy", "true");
    questionState.close.replaceChildren(makeIconSlot("loader-circle", "is-spinning"));
    questionState.close.title = "正在关闭";
    questionState.close.setAttribute("aria-label", "正在关闭");
    setQuestionControlsDisabled(questionState, true);
    try {
      await apiRequest(`/api/questions/${encodeURIComponent(questionState.id)}`, { method: "DELETE" });
      if (questionState.pending) markQuestionClosed(questionState);
    } catch (error) {
      if (!questionState.pending) return;
      const restoreFocus = questionState.restoreFocusOnClose;
      questionState.restoreFocusOnClose = false;
      questionState.closing = false;
      questionState.card.removeAttribute("aria-busy");
      questionState.error.textContent = error.message || "回答界面关闭失败";
      questionState.error.hidden = false;
      questionState.card.classList.add("is-error");
      questionState.close.replaceChildren(makeIconSlot("x"));
      questionState.close.title = "关闭回答";
      questionState.close.setAttribute("aria-label", "关闭回答");
      setQuestionControlsDisabled(questionState, false);
      updateQuestionNavigation(questionState);
      showToast(error.message || "回答界面关闭失败", "error");
      if (restoreFocus) window.requestAnimationFrame(() => questionState.close.focus());
      if ((error.status === 404 || error.status === 409) && state.viewSessionId) {
        window.setTimeout(() => loadSessionView(state.viewSessionId, { quiet: true }), 300);
      }
    }
  }

  async function submitQuestion(questionState) {
    if (!questionState.pending || questionState.submitting) return;
    let answers;
    try {
      answers = selectedQuestionAnswers(questionState);
    } catch (error) {
      const page = String(error.message || "").match(/第 (\d+) 项/);
      if (page) setQuestionPage(questionState, Number(page[1]) - 1);
      questionState.error.textContent = error.message;
      questionState.error.hidden = false;
      questionState.card.classList.add("is-error");
      return;
    }
    questionState.submitting = true;
    questionState.error.hidden = true;
    questionState.card.classList.remove("is-error");
    questionState.card.setAttribute("aria-busy", "true");
    questionState.submit.replaceChildren(makeIconSlot("loader-circle", "is-spinning"));
    questionState.submit.title = "提交中";
    questionState.submit.setAttribute("aria-label", "提交中");
    setQuestionControlsDisabled(questionState, true);
    try {
      await apiRequest(`/api/questions/${encodeURIComponent(questionState.id)}/answer`, {
        method: "POST",
        body: JSON.stringify({ answers })
      });
      if (questionState.pending) markQuestionAnswered(questionState, answers);
    } catch (error) {
      if (!questionState.pending) return;
      questionState.submitting = false;
      questionState.card.removeAttribute("aria-busy");
      questionState.error.textContent = error.message || "回答提交失败";
      questionState.error.hidden = false;
      questionState.card.classList.add("is-error");
      questionState.submit.replaceChildren(makeIconSlot("check"));
      questionState.submit.title = "提交回答";
      questionState.submit.setAttribute("aria-label", "提交回答");
      setQuestionControlsDisabled(questionState, false);
      updateQuestionNavigation(questionState);
      showToast(error.message || "回答提交失败", "error");
      if ((error.status === 404 || error.status === 409) && state.viewSessionId) {
        window.setTimeout(() => loadSessionView(state.viewSessionId, { quiet: true }), 300);
      }
    }
  }

  function createQuestion(live, data) {
    clearTypingIndicator(live, { waitingOnly: true });
    const questionId = String(data?.question_id || "");
    if (!questionId) return null;
    if (live.questions.has(questionId)) return live.questions.get(questionId);
    ensureLiveArticle(live);
    breakLiveText(live);
    finalizeLiveReasoning(live);
    live.contextOperation = null;
    const questions = Array.isArray(data?.questions) ? data.questions : [];
    const card = document.createElement("section");
    card.className = "question-card";
    card.dataset.questionId = questionId;
    const titleId = `live-question-title-${live.questions.size + 1}`;
    card.setAttribute("aria-label", "待回答问题");
    const header = document.createElement("header");
    header.hidden = true;
    const icon = document.createElement("span");
    icon.className = "question-icon";
    icon.appendChild(makeIconSlot("circle-help"));
    const headerCopy = document.createElement("div");
    const status = document.createElement("small");
    status.textContent = "等待回答";
    const title = document.createElement("strong");
    title.id = titleId;
    title.textContent = questions.length === 1 ? String(questions[0]?.header || "补充确认") : `${questions.length} 项补充确认`;
    headerCopy.append(status, title);
    header.append(icon, headerCopy);
    const form = document.createElement("form");
    form.className = "question-form";
    const heading = document.createElement("div");
    heading.className = "question-heading";
    const prompt = document.createElement("p");
    prompt.className = "question-prompt";
    prompt.id = `question-${questionId}-prompt`;
    prompt.setAttribute("aria-live", "polite");
    prompt.setAttribute("aria-atomic", "true");
    prompt.textContent = String(questions[0]?.question || questions[0]?.header || "问题 1");
    const navigation = document.createElement("div");
    navigation.className = "question-navigation";
    navigation.setAttribute("role", "group");
    navigation.setAttribute("aria-label", "问题导航");
    const previous = document.createElement("button");
    previous.type = "button";
    previous.className = "question-page-button is-previous";
    previous.title = "上一题";
    previous.setAttribute("aria-label", "上一题");
    previous.appendChild(makeIconSlot("chevron-right"));
    const position = document.createElement("span");
    position.className = "question-position";
    position.textContent = `1 of ${questions.length}`;
    position.setAttribute("aria-live", "polite");
    const next = document.createElement("button");
    next.type = "button";
    next.className = "question-page-button";
    next.title = "下一题";
    next.setAttribute("aria-label", "下一题");
    next.appendChild(makeIconSlot("chevron-right"));
    const submit = document.createElement("button");
    submit.className = "question-page-button question-submit";
    submit.type = "submit";
    submit.title = "提交回答";
    submit.setAttribute("aria-label", "提交回答");
    submit.hidden = true;
    submit.appendChild(makeIconSlot("check"));
    const close = document.createElement("button");
    close.type = "button";
    close.className = "question-page-button question-close-button";
    close.title = "关闭回答";
    close.setAttribute("aria-label", "关闭回答");
    close.appendChild(makeIconSlot("x"));
    navigation.append(previous, position, next, submit, close);
    heading.append(prompt, navigation);
    form.appendChild(heading);
    const controls = [];
    const pages = [];
    questions.forEach((question, questionIndex) => {
      const fieldset = document.createElement("fieldset");
      fieldset.className = "question-fieldset";
      fieldset.id = `question-${questionId}-page-${questionIndex + 1}`;
      fieldset.setAttribute("aria-labelledby", prompt.id);
      fieldset.hidden = questionIndex !== 0;
      const legend = document.createElement("legend");
      legend.className = "question-legend";
      legend.setAttribute("aria-hidden", "true");
      legend.textContent = String(question?.question || question?.header || `问题 ${questionIndex + 1}`);
      fieldset.appendChild(legend);
      const optionList = document.createElement("div");
      optionList.className = "question-options";
      const multiple = Boolean(question?.multiple);
      const inputType = multiple ? "checkbox" : "radio";
      const inputName = `question-${questionId}-${questionIndex}`;
      const options = [];
      for (const option of Array.isArray(question?.options) ? question.options : []) {
        const label = document.createElement("label");
        label.className = "question-option";
        const input = document.createElement("input");
        input.type = inputType;
        input.name = inputName;
        input.value = String(option?.label || "");
        input.dataset.questionIndex = String(questionIndex);
        const optionCopy = document.createElement("span");
        optionCopy.className = "question-option-copy";
        const optionLabel = document.createElement("strong");
        optionLabel.textContent = String(option?.label || "");
        optionCopy.appendChild(optionLabel);
        if (String(option?.description || "")) {
          const description = document.createElement("small");
          description.textContent = String(option.description);
          optionCopy.appendChild(description);
        }
        label.append(input, optionCopy);
        optionList.appendChild(label);
        options.push({ input, label, value: String(option?.label || "") });
      }
      fieldset.appendChild(optionList);
      let custom = null;
      if (question?.custom !== false) {
        const wrapper = document.createElement("div");
        wrapper.className = "custom-answer";
        const toggle = document.createElement("input");
        toggle.type = inputType;
        toggle.name = inputName;
        toggle.value = "__custom__";
        toggle.dataset.questionIndex = String(questionIndex);
        toggle.setAttribute("aria-label", `${question?.header || `问题 ${questionIndex + 1}`}使用自定义回答`);
        const textarea = document.createElement("textarea");
        textarea.rows = 1;
        textarea.placeholder = "自定义回答";
        textarea.setAttribute("aria-label", `${question?.header || `问题 ${questionIndex + 1}`}的自定义回答`);
        textarea.addEventListener("focus", () => {
          toggle.checked = true;
          updateQuestionOptionClasses(questionState);
        });
        textarea.addEventListener("input", () => {
          toggle.checked = Boolean(textarea.value.trim());
          updateQuestionOptionClasses(questionState);
        });
        let customNext = null;
        if (!multiple) {
          customNext = document.createElement("button");
          customNext.type = "button";
          customNext.className = "custom-answer-next";
          customNext.title = "下一题";
          customNext.setAttribute("aria-label", "下一题");
          customNext.hidden = true;
          customNext.appendChild(makeIconSlot("chevron-right"));
          customNext.addEventListener("click", () => advanceQuestion(questionState));
        }
        wrapper.append(toggle, textarea);
        if (customNext) wrapper.appendChild(customNext);
        fieldset.appendChild(wrapper);
        custom = { wrapper, toggle, textarea, next: customNext };
      }
      form.appendChild(fieldset);
      pages.push(fieldset);
      controls.push({ multiple, options, custom });
    });
    const error = document.createElement("p");
    error.className = "question-error";
    error.setAttribute("role", "alert");
    error.hidden = true;
    form.appendChild(error);
    const summary = document.createElement("dl");
    summary.className = "question-answer-summary";
    summary.hidden = true;
    card.append(header, form, summary);
    const questionState = {
      id: questionId,
      runId: live.runId,
      questions,
      card,
      header,
      titleId,
      form,
      controls,
      pages,
      pageIndex: 0,
      prompt,
      position,
      previous,
      next,
      icon,
      status,
      submit,
      close,
      error,
      summary,
      timelineParent: live.blocks,
      pending: true,
      submitting: false,
      closing: false,
      restoreFocusOnClose: false,
      autoAdvanceTimer: null,
      answers: null
    };
    form.querySelectorAll("input").forEach((input) => input.addEventListener("change", () => {
      updateQuestionOptionClasses(questionState);
      const questionIndex = Number(input.dataset.questionIndex);
      const control = questionState.controls[questionIndex];
      if (!input.checked || input.value === "__custom__" || control?.multiple || questionIndex >= questionState.pages.length - 1) return;
      window.clearTimeout(questionState.autoAdvanceTimer);
      questionState.autoAdvanceTimer = window.setTimeout(() => {
        questionState.autoAdvanceTimer = null;
        if (questionState.pageIndex !== questionIndex || !input.checked) return;
        advanceQuestion(questionState);
      }, 120);
    }));
    previous.addEventListener("click", () => setQuestionPage(questionState, questionState.pageIndex - 1, { focus: true }));
    next.addEventListener("click", () => advanceQuestion(questionState));
    close.addEventListener("click", () => closeQuestion(questionState));
    form.addEventListener("submit", (event) => {
      event.preventDefault();
      submitQuestion(questionState);
    });
    live.questions.set(questionId, questionState);
    elements.questionDock.appendChild(card);
    updateQuestionDock();
    setQuestionPage(questionState, 0);
    updateQuestionOptionClasses(questionState);
    updateControlState();
    contentAdded();
    return questionState;
  }

  function endPendingQuestions(live, message) {
    for (const question of live.questions.values()) {
      if (!question.pending) continue;
      if (question.autoAdvanceTimer) window.clearTimeout(question.autoAdvanceTimer);
      question.autoAdvanceTimer = null;
      question.pending = false;
      question.submitting = false;
      question.closing = false;
      question.restoreFocusOnClose = false;
      question.card.removeAttribute("aria-busy");
      question.card.classList.add("is-error");
      question.status.textContent = "本轮已结束";
      question.error.textContent = message;
      question.error.hidden = false;
      setQuestionControlsDisabled(question, true);
      removeQuestionFromDock(question);
    }
  }

  function createContextOperation(live, kind) {
    ensureLiveArticle(live);
    clearTypingIndicator(live, { waitingOnly: true });
    breakLiveText(live);
    finalizeLiveReasoning(live);
    const block = document.createElement("section");
    block.className = "context-operation";
    const title = document.createElement("strong");
    title.append(makeIconSlot("refresh-cw"), document.createElement("span"));
    title.lastChild.textContent = kind === "compact" ? "正在整理上下文" : "正在释放旧上下文";
    const output = document.createElement("pre");
    output.hidden = true;
    block.append(title, output);
    const operation = { kind, block, title: title.lastChild, output, raw: "" };
    live.blocks.appendChild(block);
    syncBubbleWidth(live.article);
    live.contextOperation = operation;
    contentAdded();
    return operation;
  }

  function handleContextEvent(name, live, data) {
    if (name === "context.compact_start") createContextOperation(live, "compact");
    else if (name === "context.compact_delta") {
      const operation = live.contextOperation?.kind === "compact" ? live.contextOperation : createContextOperation(live, "compact");
      operation.raw = boundedAppend(operation.raw, String(data?.delta || ""));
      operation.output.textContent = operation.raw;
      operation.output.hidden = !operation.raw;
    } else if (name === "context.compact_end") {
      if (live.contextOperation?.kind === "compact") live.contextOperation.title.textContent = "上下文已整理";
      live.contextOperation = null;
    } else if (name === "context.pop_start") createContextOperation(live, "pop");
    else if (name === "context.pop_end") {
      if (live.contextOperation?.kind === "pop") live.contextOperation.title.textContent = "旧上下文已释放";
      live.contextOperation = null;
    } else if (name === "context.error") {
      const operation = live.contextOperation || createContextOperation(live, "compact");
      operation.block.classList.add("is-error");
      operation.title.textContent = "上下文整理未完成";
      operation.raw = String(data?.message || "上下文维护失败");
      operation.output.textContent = operation.raw;
      operation.output.hidden = false;
      live.contextOperation = null;
    }
    contentAdded();
  }

  function jobStatusDisplay(status) {
    const value = String(status || "");
    if (value === "stopped") return "已中断";
    if (value === "timed_out") return "已超时";
    if (value === "exited(signal)") return "异常退出";
    if (value === "exited(0)") return "完成";
    const match = value.match(/^exited\((-?\d+)\)$/);
    return match ? `退出码 ${match[1]}` : value;
  }

  function visibleBackgroundJobs() {
    // 会话隔离: 状态条只显示当前查看会话的任务(无会话标记的旧任务保持可见)。
    return Array.from(state.backgroundJobs.values()).filter(
      (job) => !job.session_id || !state.viewSessionId || job.session_id === state.viewSessionId
    );
  }

  function renderJobsStrip() {
    const strip = elements.jobsStrip;
    if (!strip) return;
    const jobs = visibleBackgroundJobs();
    if (!jobs.length) {
      strip.hidden = true;
      strip.replaceChildren();
      updateJumpButtonOffset();
      return;
    }
    const fragment = document.createDocumentFragment();
    const collapsible = jobs.length >= 3;
    if (collapsible) {
      const toggle = document.createElement("button");
      toggle.type = "button";
      toggle.className = state.jobsStripOpen ? "jobs-strip-toggle is-open" : "jobs-strip-toggle";
      toggle.setAttribute("aria-expanded", String(state.jobsStripOpen));
      const toggleMarker = document.createElement("span");
      toggleMarker.className = "job-chip-marker is-spinning";
      toggleMarker.textContent = "\u25cc";
      const toggleText = document.createElement("span");
      toggleText.textContent = (state.jobsStripOpen ? "\u25be " : "\u25b8 ") + "\u540e\u53f0\u4efb\u52a1 \u00d7" + jobs.length;
      toggle.replaceChildren(toggleMarker, toggleText);
      toggle.addEventListener("click", () => {
        state.jobsStripOpen = !state.jobsStripOpen;
        localStorage.setItem("miyu.web.jobsStripOpen", state.jobsStripOpen ? "1" : "0");
        renderJobsStrip();
      });
      fragment.appendChild(toggle);
    }
    const showRows = !collapsible || state.jobsStripOpen;
    for (const job of showRows ? jobs : []) {
      const row = document.createElement("div");
      row.className = "job-chip";
      row.dataset.jobId = String(job.job_id);
      const marker = document.createElement("span");
      marker.className = "job-chip-marker is-spinning";
      marker.textContent = "◌";
      const label = document.createElement("span");
      label.className = "job-chip-label";
      const kindWord = job.kind === "subagent" ? "子代理" : "命令";
      label.textContent = `${kindWord} ${job.job_id} · ${job.title}`;
      label.title = label.textContent;
      const time = document.createElement("span");
      time.className = "job-chip-time";
      const seconds = job.running
        ? Math.max(0, Math.round(job.runtime_seconds + (Date.now() - job.receivedAt) / 1000))
        : job.runtime_seconds;
      time.textContent = formatJobDuration(seconds);
      const stop = document.createElement("button");
      stop.type = "button";
      stop.className = "job-chip-stop";
      stop.textContent = "✕";
      stop.title = "停止该后台命令";
      stop.addEventListener("click", async () => {
        try {
          await apiRequest(`/api/jobs/${encodeURIComponent(job.job_id)}`, { method: "DELETE" });
        } catch (error) {
          showToast(error.message || "停止失败", "error");
        }
      });
      row.append(marker, label, time, stop);
      fragment.appendChild(row);
    }
    strip.replaceChildren(fragment);
    strip.hidden = false;
    updateJumpButtonOffset();
  }

  function formatJobDuration(seconds) {
    const value = Math.max(0, Math.floor(seconds));
    if (value >= 3600) return `${Math.floor(value / 3600)}h ${String(Math.floor((value % 3600) / 60)).padStart(2, "0")}m`;
    if (value >= 60) return `${Math.floor(value / 60)}m ${String(value % 60).padStart(2, "0")}s`;
    return `${value}s`;
  }

  async function seedJobsStrip() {
    try {
      const data = await apiRequest("/api/jobs");
      state.backgroundJobs.clear();
      for (const job of data?.jobs || []) {
        state.backgroundJobs.set(String(job.job_id), { ...job, receivedAt: Date.now() });
      }
      renderJobsStrip();
    } catch {
      /* daemon may predate the jobs API */
    }
  }

  setInterval(() => {
    const visible = visibleBackgroundJobs();
    if (!visible.length) return;
    // 只更新计时文本：全量重建会重启 CSS 旋转动画，导致 spinner 每秒瞬移回原点。
    let missing = false;
    for (const job of visible) {
      const row = elements.jobsStrip?.querySelector(`.job-chip[data-job-id="${CSS.escape(String(job.job_id))}"]`);
      if (!row) {
        missing = true;
        continue;
      }
      const time = row.querySelector(".job-chip-time");
      if (!time) continue;
      const seconds = Math.max(0, Math.round(job.runtime_seconds + (Date.now() - job.receivedAt) / 1000));
      time.textContent = formatJobDuration(seconds);
    }
    if (missing && (state.jobsStripOpen || visible.length < 3)) renderJobsStrip();
  }, 1000);
  setTimeout(seedJobsStrip, 800);

  function appendRunNotice(live, message, error = false) {
    ensureLiveArticle(live);
    clearTypingIndicator(live);
    breakLiveText(live);
    const notice = document.createElement("div");
    notice.className = `run-notice${error ? " is-error" : ""}`;
    notice.append(makeIconSlot(error ? "circle-alert" : "circle-stop"));
    const text = document.createElement("span");
    text.textContent = String(message || "");
    notice.appendChild(text);
    live.blocks.appendChild(notice);
  }

  function markUnfinishedTools(live) {
    for (const tool of live.tools.values()) {
      if (tool.finished) continue;
      tool.finished = true;
      tool.finishedAt = performance.now();
      updateToolStatus(tool, "已中断", "circle-alert", "is-failure");
      updateToolSummary(tool);
      if (tool.liveProgress) {
        if (tool.liveProgress.textContent.trim()) tool.liveProgress.classList.add("is-error");
        else tool.liveProgress.hidden = true;
        tool.progressDetail.wrapper.hidden = !tool.progressDetail.raw;
        syncBubbleWidth(live.article);
      }
      if (!state.toolExpanded) {
        tool.card.classList.add("collapsed");
        tool.head.setAttribute("aria-expanded", "false");
      }
    }
  }

  function setLiveEndpoint(live, providerId, model) {
    const values = [providerId, model].map((value) => String(value || "").trim()).filter(Boolean);
    live.providerId = String(providerId || "");
    live.model = String(model || "");
    if (!live.endpoint) return;
    live.endpoint.textContent = values.join(" / ");
    live.endpoint.hidden = !state.display?.show_mixed_model_endpoint || values.length === 0;
  }

  function consumeLiveQueue(live, data) {
    finalizeLiveReasoning(live);
    setLiveEndpoint(live, data?.provider_id, data?.model);
    if (live.headerStatus) live.headerStatus.textContent = "刚刚";
    if (live.meta) live.meta.textContent = "已完成";

    const ids = new Set((Array.isArray(data?.prompt_ids) ? data.prompt_ids : []).map(String));
    const consumed = state.queuedPrompts.filter((prompt) => ids.has(String(prompt?.id)));
    state.queuedPrompts = state.queuedPrompts.filter((prompt) => !ids.has(String(prompt?.id)));
    for (const prompt of consumed) {
      elements.timeline.appendChild(createUserMessage(prompt?.content || "", prompt?.submitted_at || new Date(), {
        turnId: live.turnId,
        runId: live.runId,
        followupId: prompt?.id,
        attachments: prompt?.attachments
      }));
    }
    renderQueueTray();

    stashLiveArticle(live, "segment");
    removeLiveStopButton(live);
    live.article = null;
    live.blocks = null;
    live.headerStatus = null;
    live.meta = null;
    live.endpoint = null;
    live.copyButton = null;
    live.streamRail = null;
    live.typingAnimation = null;
    live.currentText = null;
    live.assistantText = "";
    live.assistantReasoning = "";
    live.reasoning = null;
    live.reasoningParts = [];
    live.reasoningStarted = false;
    live.reasoningTitle = "";
    live.tools = new Map();
    live.questions = new Map();
    live.contextOperation = null;
    showTypingIndicator(live);
    contentAdded();
  }

  function updateLocalTurnFromLive(live, terminalStatus, data) {
    const status = terminalStatus === "completed" ? "completed" : "interrupted";
    let turn = live.turnId ? state.turns.find((item) => String(item?.id) === String(live.turnId)) : null;
    if (!turn && (live.userText || live.userAttachments.length)) {
      turn = {
        id: live.turnId || `local-${live.runId}`,
        seq: state.turns.length ? Math.max(...state.turns.map((item) => asFiniteNumber(item?.seq))) + 1 : 1,
        status,
        active_context: true,
        user_content: live.userText,
        assistant_content: live.assistantText,
        assistant_reasoning: live.assistantReasoning || null,
        provider_id: data?.provider_id || live.providerId || null,
        model: data?.model || live.model || null,
        user_timestamp: new Date().toISOString(),
        assistant_timestamp: new Date().toISOString(),
        token_total: effectiveUsageTotal(data?.usage),
        token_usage_estimated: Boolean(data?.usage_estimated),
        question_exchanges: [],
        followups: [],
        assets: [...live.assets],
        artifacts: [...live.artifacts],
        attachments: [...live.userAttachments]
      };
      state.turns.push(turn);
    } else if (turn) {
      turn.status = status;
      if (live.assistantText.trim()) turn.assistant_content = live.assistantText;
      if (live.assistantReasoning.trim()) turn.assistant_reasoning = live.assistantReasoning;
      if (data?.provider_id || live.providerId) turn.provider_id = data?.provider_id || live.providerId;
      if (data?.model || live.model) turn.model = data?.model || live.model;
      if (live.assets.length) turn.assets = [...live.assets];
      if (live.artifacts.length) turn.artifacts = [...live.artifacts];
      turn.assistant_timestamp = new Date().toISOString();
      if (terminalStatus === "completed") {
        turn.token_total = effectiveUsageTotal(data?.usage);
        turn.token_usage_estimated = Boolean(data?.usage_estimated);
      }
    }
  }

  // 回合内一次模型请求结束(chat.round_usage):立即刷新气泡计量与上下文
  // 条,不等 run 完结。usage 是刚结束请求的用量,其 prompt+completion 即
  // 当前上下文占用;turn_* 是回合累计。回合结束后 finishLiveRun 会用权威
  // 数字覆盖这里的中间值。
  function handleRoundUsage(live, data) {
    if (live.meta) {
      const usage = formatUsageMeta({
        turnTotal: asFiniteNumber(data?.turn_total),
        turnPrompt: data?.turn_prompt,
        turnCached: data?.turn_cache_read,
        estimated: data?.estimated
      });
      if (usage) live.meta.textContent = usage;
    }
    const round = data?.usage;
    const contextTokens = asFiniteNumber(round?.prompt_tokens, 0) + asFiniteNumber(round?.completion_tokens, 0);
    if (contextTokens > 0) {
      state.context.tokens = contextTokens;
      updateContext();
    }
  }

  function finishLiveRun(kind, data, live) {
    if (!live || live.ended) return;
    const runId = live.runId;
    if (live.operation === "redo" && kind !== "completed") {
      live.ended = true;
      disposeLiveState(live);
      state.liveRuns.delete(runId);
      state.replayRunIds?.delete(runId);
      state.terminalRunIds.add(runId);
      showToast(kind === "failed" ? String(data?.message || "重新生成失败") : "重新生成已取消", "error");
      if (state.viewSessionId) loadSessionView(state.viewSessionId, { quiet: true });
      updateConversationChrome();
      updateControlState();
      return;
    }
    live.ended = true;
    clearPreparingTool(live);
    clearTypingIndicator(live);
    finalizeLiveReasoning(live);
    setLiveEndpoint(live, data?.provider_id, data?.model);
    removeLiveStopButton(live);
    state.terminalRunIds.add(runId);
    if (state.terminalRunIds.size > 30) state.terminalRunIds.delete(state.terminalRunIds.values().next().value);

    if (kind === "completed") {
      if (live.headerStatus) live.headerStatus.textContent = "刚刚";
      if (live.meta) {
        const usage = formatUsageMeta({
          turnTotal: effectiveUsageTotal(data?.usage),
          turnPrompt: data?.usage?.prompt_tokens,
          turnCached: data?.usage?.cache_read_tokens,
          estimated: data?.usage_estimated,
          cumulative: data?.cumulative_tokens,
          cumulativePrompt: data?.cumulative_prompt_tokens,
          cumulativeCached: data?.cumulative_cache_read_tokens
        });
        live.meta.textContent = usage || "已完成";
      }
    } else if (kind === "cancelled") {
      markUnfinishedTools(live);
      endPendingQuestions(live, "本轮已停止，无法再提交回答");
      // 停止状态只由时间线的「本轮已中断」一处表达,气泡内通知与 header/meta 不再重复
      if (live.headerStatus) live.headerStatus.textContent = "";
      if (live.meta) live.meta.textContent = "";
    } else {
      markUnfinishedTools(live);
      endPendingQuestions(live, "本轮已结束，无法再提交回答");
      appendRunNotice(live, String(data?.message || "本轮运行失败"), true);
      if (live.headerStatus) live.headerStatus.textContent = "运行失败";
      if (live.meta) live.meta.textContent = "";
    }

    updateLocalTurnFromLive(live, kind, data);
    stashLiveArticle(live, "final");
    if (kind === "completed" || kind === "cancelled") {
      // 上下文条展示全局（默认会话）上下文；其他会话的 run 不覆盖它。
      // cancelled 也要刷新：被中断的轮次已经持久化进上下文。
      const updatesGlobalContext = !data?.session_id || String(data.session_id) === String(state.currentSessionId || "");
      if (updatesGlobalContext) {
        if (data?.context_tokens != null) state.context.tokens = Math.max(0, asFiniteNumber(data.context_tokens));
        state.context.window = data?.context_window == null ? state.context.window : Math.max(0, asFiniteNumber(data.context_window));
      }
      const usage = data?.usage && typeof data.usage === "object" ? data.usage : null;
      if (usage) {
        state.usage.last_usage = usage;
        state.usage.last_conversation_usage = usage;
        state.usage.requests = asFiniteNumber(state.usage.requests) + 1;
        state.usage.prompt_tokens = asFiniteNumber(state.usage.prompt_tokens) + asFiniteNumber(usage.prompt_tokens);
        state.usage.completion_tokens = asFiniteNumber(state.usage.completion_tokens) + asFiniteNumber(usage.completion_tokens);
        state.usage.total_tokens = asFiniteNumber(state.usage.total_tokens) + effectiveUsageTotal(usage);
        state.usage.cache_read_tokens = asFiniteNumber(state.usage.cache_read_tokens) + asFiniteNumber(usage.cache_read_tokens, 0);
        state.usage.cache_write_tokens = asFiniteNumber(state.usage.cache_write_tokens) + asFiniteNumber(usage.cache_write_tokens, 0);
      }
    }
    state.liveRuns.delete(runId);
    state.replayRunIds?.delete(runId);
    state.pendingSubmission = null;
    updateContext();
    updateRuntimeUsage(data?.usage || null, Boolean(data?.usage_estimated));
    updateConversationChrome();
    updateControlState();
    contentAdded();
    if (state.liveRuns.size === 0) {
      window.requestAnimationFrame(() => {
        if (!state.blocked && !elements.settingsDrawer.classList.contains("open")) focusComposerIfDesktop();
      });
      window.setTimeout(() => {
        if (state.liveRuns.size === 0) refreshViewSnapshot();
      }, 120);
    }
  }

  function clearViewSyncTimer() {
    if (!state.viewSyncTimer) return;
    window.clearTimeout(state.viewSyncTimer);
    state.viewSyncTimer = null;
  }

  function scheduleViewSync() {
    clearViewSyncTimer();
    if (!state.viewRunningTurnId || state.blocked) return;
    state.viewSyncTimer = window.setTimeout(() => {
      state.viewSyncTimer = null;
      refreshViewSnapshot();
    }, 1_000);
  }

  async function refreshViewSnapshot() {
    const sessionId = state.viewSessionId;
    if (!sessionId || state.blocked || state.viewLoading || state.resyncing) {
      scheduleViewSync();
      return;
    }
    try {
      const response = await apiRequest(`/api/sessions/${encodeURIComponent(sessionId)}/turns`);
      const payload = await response.json();
      if (state.viewSessionId !== sessionId || state.viewLoading) return;
      const runs = (Array.isArray(payload?.runs) ? payload.runs : []).filter((run) => run?.run_id);
      if (runs.length) state.runsBySession.set(sessionId, new Set(runs.map((run) => String(run.run_id))));
      else if (state.liveRuns.size === 0) state.runsBySession.delete(sessionId);
      state.viewRunningTurnId = !runs.length && typeof payload?.running_turn_id === "string" && payload.running_turn_id
        ? payload.running_turn_id
        : null;
      if (state.liveRuns.size === 0) {
        const nextTurns = Array.isArray(payload?.turns)
          ? payload.turns.sort((a, b) => asFiniteNumber(a?.seq) - asFiniteNumber(b?.seq))
          : state.turns;
        const turnsChanged = JSON.stringify(nextTurns) !== JSON.stringify(state.turns);
        const nextCandidate = payload?.redo_candidate && typeof payload.redo_candidate === "object"
          ? payload.redo_candidate
          : null;
        const candidateChanged = JSON.stringify(nextCandidate) !== JSON.stringify(state.redoCandidate);
        state.turns = nextTurns;
        state.queuedPrompts = Array.isArray(payload?.queued_prompts) ? payload.queued_prompts : state.queuedPrompts;
        state.redoCandidate = nextCandidate;
        if (turnsChanged || candidateChanged) renderConversation();
        renderQueueTray();
        restoreLiveRuns(runs);
      }
      renderSessionList();
      updateConversationChrome();
      updateControlState();
    } catch (error) {
      if (error.status === 401) {
        showBlockedState(true);
        return;
      }
      if (error.status === 404) {
        state.viewRunningTurnId = null;
        refreshSessions();
        return;
      }
    } finally {
      scheduleViewSync();
    }
  }

  async function ensureActiveTurnUser(live, turnId) {
    if (!live || live.userRendered || !turnId) return;
    const existing = state.turns.find((turn) => String(turn?.id) === String(turnId));
    if (existing) {
      live.userText = String(existing.user_content || "");
      live.userRendered = true;
      updateConversationChrome();
      return;
    }
    const sessionId = state.viewSessionId;
    try {
      const response = await apiRequest(`/api/sessions/${encodeURIComponent(sessionId)}/turns`);
      const payload = await response.json();
      if (state.viewSessionId !== sessionId || state.liveRuns.get(live.runId) !== live || live.userRendered) return;
      const turn = Array.isArray(payload?.turns) ? payload.turns.find((item) => String(item?.id) === String(turnId)) : null;
      if (!turn) return;
      live.userText = String(turn.user_content || "");
      live.userAttachments = Array.isArray(turn.attachments) ? turn.attachments : [];
      ensureLiveUser(live, live.userText);
    } catch (_) {
      // The stream can continue; a later view refresh will recover the user turn.
    }
  }

  function handleRunEvent(name, data) {
    const runId = String(data?.run_id || "");
    if (!runId) return;
    const sessionId = typeof data?.session_id === "string" && data.session_id ? data.session_id : runSessionId(runId);
    const terminal = name === "run.completed" || name === "run.cancelled" || name === "run.failed";
    if (name === "run.started" && sessionId) trackRun(sessionId, runId);

    let live = state.liveRuns.get(runId);
    if (!live && !terminal && !state.terminalRunIds.has(runId) && sessionId && sessionId === state.viewSessionId) {
      // 视图会话里出现的新 run（本端发起、他端发起或重放）都会挂上 live 块。
      // run.started 意味着全新的 turn，不去认领时间线里已有的 running turn。
      live = createLiveForRun(runId, "", {
        claimTurn: name !== "run.started",
        operation: String(data?.operation || "create"),
        turnId: String(data?.turn_id || "") || null,
        inputId: String(data?.input_id || "") || null
      });
      if (live.turnId && state.viewRunningTurnId === String(live.turnId)) state.viewRunningTurnId = null;
    }

    if (name === "run.started") {
      if (live) {
        live.operation = String(data?.operation || live.operation || "create");
        live.turnId = String(data?.turn_id || live.turnId || "") || null;
        live.inputId = String(data?.input_id || live.inputId || "") || null;
      }
      if (live && !live.ended && live.operation !== "redo") showTypingIndicator(live);
      renderSessionList();
      updateConversationChrome();
      updateControlState();
      return;
    }
    if (terminal) {
      untrackRun(runId);
      if (live) {
        finishLiveRun(name.slice("run.".length), data, live);
      } else {
        state.terminalRunIds.add(runId);
        if (state.terminalRunIds.size > 30) state.terminalRunIds.delete(state.terminalRunIds.values().next().value);
        if (name === "run.completed" && data?.session_id && String(data.session_id) === String(state.currentSessionId || "")) {
          if (data?.context_tokens != null) state.context.tokens = Math.max(0, asFiniteNumber(data.context_tokens));
          state.context.window = data?.context_window == null ? state.context.window : Math.max(0, asFiniteNumber(data.context_window));
          updateContext();
        }
        renderSessionList();
      }
      return;
    }
    if (!live) return;

    if (name === "turn.started") {
      live.turnId = String(data?.turn_id || "");
      if (live.article) live.article.dataset.turnId = live.turnId;
      if (String(data?.operation || "") === "redo") {
        live.operation = "redo";
        live.inputId = String(data?.input_id || live.inputId || "") || null;
        if (typeof data?.display_content === "string") live.editedContent = data.display_content;
      }
      if (state.viewRunningTurnId === live.turnId) state.viewRunningTurnId = null;
      removeRunningStatus(live.turnId);
      if (live.operation === "redo") commitRedoLive(live);
      else ensureActiveTurnUser(live, live.turnId);
    } else if (name === "assistant.delta") appendAssistantDelta(live, data?.delta);
    else if (name === "chat.round_usage") handleRoundUsage(live, data);
    else if (name === "generation.superseded") resetSupersededGeneration(live);
    else if (name.startsWith("reasoning.")) handleReasoningEvent(name, live, data);
    else if (name === "queue.consumed") consumeLiveQueue(live, data);
    else if (name.startsWith("tool.")) handleToolEvent(name, live, data);
    else if (name === "question.requested") {
      clearPreparingTool(live);
      createQuestion(live, data);
    }
    else if (name === "question.answered") {
      const question = live.questions.get(String(data?.question_id || ""));
      if (question) markQuestionAnswered(question, data?.answers);
    } else if (name === "question.closed") {
      const question = live.questions.get(String(data?.question_id || ""));
      if (question) markQuestionClosed(question);
    } else if (name.startsWith("context.")) handleContextEvent(name, live, data);
  }

  function eventShouldBeHandled(name, data, eventId) {
    if (name === "resync_required") {
      if (eventId > 0) state.lastEventId = eventId;
      return true;
    }
    if (eventId > 0 && eventId <= state.lastEventId) return false;
    if (eventId > 0) state.lastEventId = eventId;
    if (state.replayRunIds && eventId > 0 && eventId <= state.replayCutoff) {
      // 重放窗口内只重建正在恢复的 run，其余事件已经反映在快照里。
      if (!RUN_EVENTS.has(name)) return false;
      return state.replayRunIds.has(String(data?.run_id || ""));
    }
    if (state.replayRunIds && eventId > state.replayCutoff) state.replayRunIds = null;
    return true;
  }

  function handleSseEvent(name, event) {
    let data;
    try {
      data = event.data ? JSON.parse(event.data) : {};
    } catch (_) {
      showToast("收到无法解析的事件，正在重新同步", "error");
      loadBootstrap();
      return;
    }
    const eventId = Math.max(0, asFiniteNumber(event.lastEventId));
    if (!eventShouldBeHandled(name, data, eventId)) return;
    if (name === "resync_required") {
      if (state.replayRunIds) {
        state.replayResyncCount += 1;
        state.replayResyncAt = Date.now();
      } else {
        state.replayResyncCount = 0;
      }
      if (!state.resyncing) {
        state.resyncing = true;
        loadBootstrap().finally(() => {
          state.resyncing = false;
        });
      }
      return;
    }
    if (name.startsWith("session.")) {
      handleSessionEvent(name, data);
      return;
    }
    if (name === "queue.added") {
      const prompt = data?.prompt;
      if (queueEventTargetsView(data) && prompt && !state.queuedPrompts.some((item) => String(item?.id) === String(prompt?.id))) {
        state.queuedPrompts.push(prompt);
        renderQueueTray();
      }
      return;
    }
    if (name === "job.started") {
      const job = data?.job;
      if (job?.job_id) {
        state.backgroundJobs.set(String(job.job_id), { ...job, receivedAt: Date.now() });
        renderJobsStrip();
      }
      return;
    }
    if (name === "job.finished") {
      if (state.backgroundJobs.delete(String(data?.job_id))) renderJobsStrip();
      return;
    }
    if (name === "job.acknowledged") {
      if (state.backgroundJobs.delete(String(data?.job_id))) renderJobsStrip();
      return;
    }
    if (name === "queue.removed") {
      if (queueEventTargetsView(data)) {
        state.queuedPrompts = state.queuedPrompts.filter((prompt) => String(prompt?.id) !== String(data?.prompt_id));
        renderQueueTray();
      }
      return;
    }
    if (name === "conversation.reset" || name === "conversation.pop") {
      const sessionId = typeof data?.session_id === "string" ? data.session_id : "";
      if (sessionId && sessionId !== state.viewSessionId) {
        refreshSessions();
      } else if (!state.viewSessionId || state.viewSessionId === state.currentSessionId) {
        loadBootstrap();
      } else {
        loadSessionView(state.viewSessionId, { quiet: true });
        refreshSessions();
      }
      return;
    }
    handleRunEvent(name, data);
  }

  function queueEventTargetsView(data) {
    const explicit = typeof data?.session_id === "string" && data.session_id ? data.session_id : "";
    if (explicit) return explicit === state.viewSessionId;
    const runId = String(data?.run_id || "");
    if (runId) {
      if (state.liveRuns.has(runId)) return true;
      const sessionId = runSessionId(runId);
      if (sessionId) return sessionId === state.viewSessionId;
    }
    const turnId = String(data?.turn_id || "");
    if (turnId) {
      if (state.viewRunningTurnId && turnId === state.viewRunningTurnId) return true;
      for (const live of state.liveRuns.values()) {
        if (String(live.turnId || "") === turnId) return true;
      }
      return state.turns.some((turn) => String(turn?.id) === turnId && turn?.status === "running");
    }
    return false;
  }

  function closeEventSource() {
    if (state.eventSource) {
      state.eventSource.close();
      state.eventSource = null;
    }
    if (state.healthTimer) {
      window.clearTimeout(state.healthTimer);
      state.healthTimer = null;
    }
  }

  async function refineConnectionHealth(source) {
    if (state.eventSource !== source || source.readyState === EventSource.OPEN) return;
    try {
      const response = await fetch("/api/health", { cache: "no-store", credentials: "same-origin" });
      if (!response.ok) throw new Error("health check failed");
      if (state.eventSource === source && source.readyState !== EventSource.OPEN) setConnectionStatus("connecting");
    } catch (_) {
      if (state.eventSource === source && source.readyState !== EventSource.OPEN) setConnectionStatus("offline");
    }
  }

  function connectEventSource(after) {
    closeEventSource();
    if (state.blocked) return;
    const source = new EventSource(`/api/events?after=${encodeURIComponent(Math.max(0, asFiniteNumber(after)))}`);
    state.eventSource = source;
    source.onopen = () => {
      if (state.eventSource !== source) return;
      setConnectionStatus("online");
      if (state.healthTimer) window.clearTimeout(state.healthTimer);
      state.healthTimer = null;
    };
    source.onerror = () => {
      if (state.eventSource !== source) return;
      setConnectionStatus("connecting");
      if (state.healthTimer) window.clearTimeout(state.healthTimer);
      state.healthTimer = window.setTimeout(() => refineConnectionHealth(source), 1200);
    };
    for (const name of EVENT_NAMES) source.addEventListener(name, (event) => handleSseEvent(name, event));
  }

  function showBlockedState(unauthorized, message = "") {
    state.blocked = true;
    state.viewRunningTurnId = null;
    clearViewSyncTimer();
    disposeAllLiveRuns();
    clearQuestionDock();
    closeEventSource();
    elements.loadingState.hidden = true;
    elements.timeline.hidden = true;
    elements.emptyState.hidden = true;
    elements.blockedState.hidden = false;
    elements.blockedTitle.textContent = unauthorized ? "登录 Miyu" : "无法载入 Miyu WebUI";
    elements.blockedMessage.textContent = unauthorized ? "输入访问密码以继续。" : message || "本地服务暂时无法访问";
    elements.loginForm.hidden = !unauthorized;
    elements.retryBootstrapButton.hidden = unauthorized;
    elements.loginError.textContent = "";
    elements.loginError.hidden = true;
    setLoginSubmitting(false);
    setConnectionStatus(unauthorized ? "blocked" : "offline");
    updateControlState();
    if (unauthorized) window.requestAnimationFrame(() => elements.loginPassword.focus());
  }

  function applyBootstrap(snapshot) {
    state.blocked = false;
    clearViewSyncTimer();
    disposeAllLiveRuns();
    state.bootId = String(snapshot?.boot_id || "");
    state.latestEventId = Math.max(0, asFiniteNumber(snapshot?.latest_event_id));
    state.models = Array.isArray(snapshot?.models) ? snapshot.models : [];
    applyPersona(snapshot?.persona);
    state.display = snapshot?.display && typeof snapshot.display === "object" ? snapshot.display : state.display;
    state.context = snapshot?.context && typeof snapshot.context === "object" ? snapshot.context : { tokens: 0, window: null };
    state.usage = snapshot?.usage && typeof snapshot.usage === "object" ? snapshot.usage : {};
      state.capabilities = snapshot?.capabilities && typeof snapshot.capabilities === "object" ? snapshot.capabilities : {};
    state.sessions = Array.isArray(snapshot?.sessions) ? snapshot.sessions : [];
    state.currentSessionId = typeof snapshot?.current_session_id === "string" && snapshot.current_session_id ? snapshot.current_session_id : null;
    state.sessionMenuFor = null;
    state.sessionRenaming = null;
    state.version = snapshot?.version ?? null;
    state.pendingSubmission = null;
    const allRuns = (Array.isArray(snapshot?.runs) ? snapshot.runs : []).filter((run) => run?.run_id && run?.session_id);
    state.runsBySession = new Map();
    for (const run of allRuns) trackRun(String(run.session_id), String(run.run_id));
    elements.loginForm.hidden = true;
    elements.retryBootstrapButton.hidden = false;
    elements.loginPassword.value = "";
    elements.loginError.textContent = "";
    elements.loginError.hidden = true;
    setLoginSubmitting(false);
    elements.versionLabel.textContent = state.version ? `v${state.version}` : "--";
    clearInlineError();
    renderModelMenu();
    updateCapabilities();
    updateContext();
    state.replayRunIds = null;
    state.replayCutoff = 0;
    const keepView = state.viewSessionId && state.viewSessionId !== state.currentSessionId && findSession(state.viewSessionId);
    if (keepView) {
      // 视图停留在非默认会话：全局重载不改变浏览位置，改用会话接口回填。
      state.lastEventId = state.latestEventId;
      connectEventSource(state.latestEventId);
      loadSessionView(state.viewSessionId, { quiet: true });
    } else if (state.currentSessionId) {
      applySessionView({
        session_id: state.currentSessionId,
        turns: snapshot?.turns,
        queued_prompts: snapshot?.queued_prompts,
        running_turn_id: snapshot?.running_turn_id,
        runs: allRuns.filter((run) => String(run.session_id) === String(state.currentSessionId)),
        redo_candidate: snapshot?.redo_candidate
      });
      if (state.liveRuns.size === 0) {
        state.lastEventId = state.latestEventId;
        connectEventSource(state.latestEventId);
      }
    } else {
      // 单会话兜底：没有会话指针时直接使用 bootstrap 快照。
      state.viewSessionId = null;
      state.sessionModelOverride = null;
      state.sessionModelOverrideFor = "";
      updateCurrentModelDisplay();
      state.viewRunningTurnId = typeof snapshot?.running_turn_id === "string" && snapshot.running_turn_id ? snapshot.running_turn_id : null;
      state.turns = Array.isArray(snapshot?.turns) ? snapshot.turns.sort((a, b) => asFiniteNumber(a?.seq) - asFiniteNumber(b?.seq)) : [];
      state.queuedPrompts = Array.isArray(snapshot?.queued_prompts) ? snapshot.queued_prompts : [];
      state.redoCandidate = snapshot?.redo_candidate && typeof snapshot.redo_candidate === "object"
        ? snapshot.redo_candidate
        : null;
      renderConversation();
      renderQueueTray();
      state.lastEventId = state.latestEventId;
      connectEventSource(state.latestEventId);
    }
    setConnectionStatus("connecting");
    updateRuntimeUsage();
    updateConversationChrome();
    updateControlState();
    loadThinkingVariants();
  }

  async function loadBootstrap() {
    if (state.bootstrapPromise) return state.bootstrapPromise;
    state.bootstrapPromise = (async () => {
      clearViewSyncTimer();
      closeEventSource();
      state.adminBusy = false;
      state.submitting = false;
      if (!state.turns.length && state.liveRuns.size === 0) {
        elements.loadingState.hidden = false;
        elements.blockedState.hidden = true;
        elements.emptyState.hidden = true;
        elements.timeline.hidden = true;
      }
      setConnectionStatus("connecting");
      updateControlState();
      try {
        const response = await apiRequest("/api/bootstrap");
        const snapshot = await response.json();
        applyBootstrap(snapshot);
      } catch (error) {
        showBlockedState(error.status === 401, error.message);
      }
    })();
    try {
      await state.bootstrapPromise;
    } finally {
      state.bootstrapPromise = null;
    }
  }

  function setLoginSubmitting(submitting) {
    state.loginSubmitting = Boolean(submitting);
    elements.loginPassword.disabled = state.loginSubmitting;
    elements.loginSubmit.disabled = state.loginSubmitting;
    elements.loginSubmit.classList.toggle("is-loading", state.loginSubmitting);
    elements.loginSubmitLabel.textContent = state.loginSubmitting ? "正在登录" : "登录";
    const icon = elements.loginSubmit.querySelector(".icon-slot");
    if (icon) icon.replaceChildren(createIcon(state.loginSubmitting ? "loader-circle" : "log-in"));
  }

  async function submitLogin() {
    if (state.loginSubmitting) return;
    const password = elements.loginPassword.value;
    if (!password) {
      elements.loginError.textContent = "请输入访问密码";
      elements.loginError.hidden = false;
      elements.loginPassword.focus();
      return;
    }
    elements.loginError.textContent = "";
    elements.loginError.hidden = true;
    setLoginSubmitting(true);
    try {
      await apiRequest("/api/auth/login", {
        method: "POST",
        body: JSON.stringify({ password })
      });
      elements.loginPassword.value = "";
      await loadBootstrap();
    } catch (error) {
      elements.loginError.textContent = error.status === 401 ? "密码不正确，请重试" : error.message || "登录失败";
      elements.loginError.hidden = false;
      window.requestAnimationFrame(() => {
        elements.loginPassword.focus();
        elements.loginPassword.select();
      });
    } finally {
      setLoginSubmitting(false);
    }
  }

  async function confirmModelSelection() {
    if (!(state.stagedModelKeys instanceof Set) || state.modelSelectionSubmitting) return;
    const sessionId = String(state.viewSessionId || state.currentSessionId || "");
    if (!sessionId) {
      state.modelMenuError = "当前视图没有可设置的会话";
      updateModelMenuState();
      return;
    }
    const follow = state.stagedFollowGlobal || state.stagedModelKeys.size === 0;
    const selected = follow ? [] : state.models.filter((model) => state.stagedModelKeys.has(modelKey(model)));
    if (!follow && selected.length === 0) {
      state.modelMenuError = "所选模型已不可用，请重新选择";
      updateModelMenuState();
      return;
    }
    state.modelSelectionSubmitting = true;
    state.modelMenuError = "";
    clearInlineError();
    updateModelMenuState();
    let applied = false;
    try {
      const response = await apiRequest(`/api/sessions/${encodeURIComponent(sessionId)}/models`, {
        method: "PUT",
        body: JSON.stringify({
          models: selected.map((model) => ({
            provider_id: String(model.provider_id || ""),
            model: String(model.model || "")
          }))
        })
      });
      const payload = await response.json();
      applied = true;
      state.modelSelectionSubmitting = false;
      closeModelMenu();
      setSessionModelOverride(sessionId, payload?.model_override);
      showToast(follow ? "本会话已恢复跟随全局" : "本会话模型已更新（下一轮生效）");
    } catch (error) {
      state.modelMenuError = error.message || "模型设置未保存";
      showInlineError(error.message);
      showToast(error.message, "error");
    } finally {
      state.modelSelectionSubmitting = false;
      updateControlState();
      if (applied) window.requestAnimationFrame(() => elements.modelButton.focus());
      else {
        updateModelMenuState();
        window.requestAnimationFrame(() => elements.modelMenu.querySelector(".model-confirm")?.focus());
      }
    }
  }

  async function submitTurn() {
    if (state.adminBusy || state.submitting || state.blocked) return;
    if (hasPendingQuestion()) return;
    const sessionId = state.viewSessionId;
    const queueing = conversationRunning();
    const updateTarget = queueing ? activeTurnUpdateTarget(sessionId) : null;
    const content = elements.composerInput.value.trim();
    const readyAttachments = state.composerAttachments.filter((item) => item.status === "ready");
    const attachmentIds = readyAttachments.map((item) => item.id);
    const sentAttachments = readyAttachments.map((item) => ({
      id: item.id,
      url: item.url,
      name: item.name,
      mime: item.mime,
      kind: item.kind,
      size: item.size,
      width: item.width || 0,
      height: item.height || 0
    }));
    const count = countCharacters(content);
    if (!content && !attachmentIds.length) {
      elements.composerState.textContent = "消息不能为空";
      elements.composerState.classList.add("is-error");
      return;
    }
    if (count > MAX_CONTENT_CHARS) {
      elements.composerState.textContent = "消息不能超过 20,000 个字符";
      elements.composerState.classList.add("is-error");
      return;
    }
    if (queueing && !updateTarget) {
      elements.composerState.textContent = "当前存在多个回复或回复仍在启动，无法确定追加目标";
      elements.composerState.classList.add("is-error");
      return;
    }
    state.submitting = true;
    if (!queueing) state.pendingSubmission = { content, attachments: sentAttachments };
    clearInlineError();
    updateControlState();
    try {
      const body = queueing
        ? { content, run_id: updateTarget.runId, turn_id: updateTarget.turnId, attachment_ids: attachmentIds }
        : { content, attachment_ids: attachmentIds };
      if (sessionId) body.session_id = sessionId;
      const response = await apiRequest(queueing ? "/api/queue" : "/api/turns", {
        method: "POST",
        body: JSON.stringify(body)
      });
      const payload = await response.json();
      const queuedPrompt = queueing ? payload : payload?.queued ? payload.prompt : null;
      if (queuedPrompt) {
        if (!state.queuedPrompts.some((prompt) => String(prompt?.id) === String(queuedPrompt?.id))) {
          state.queuedPrompts.push(queuedPrompt);
        }
        state.pendingSubmission = null;
        elements.composerInput.value = "";
        committedComposerAttachments();
        resizeComposer();
        renderQueueTray();
        if (!queueing) {
          // 服务端发现该会话已有 turn 在运行并自动转排队：同步该 run 的 live 状态。
          const runningRunId = String(payload?.run_id || "");
          if (runningRunId && sessionId) {
            trackRun(sessionId, runningRunId);
            if (!state.liveRuns.has(runningRunId) && !state.terminalRunIds.has(runningRunId)) {
              createLiveForRun(runningRunId);
              beginRunReplay();
            }
          } else {
            state.viewRunningTurnId = String(payload?.running_turn_id || "") || state.viewRunningTurnId;
            scheduleViewSync();
          }
          renderSessionList();
          updateConversationChrome();
        }
        return;
      }
      const runId = String(payload?.run_id || "");
      if (!runId) throw new ApiError("服务未返回运行标识", response.status);
      if (state.terminalRunIds.has(runId)) {
        if (sessionId) await loadSessionView(sessionId, { quiet: true });
        else await loadBootstrap();
      } else {
        if (sessionId) trackRun(sessionId, runId);
        const live = createLiveForRun(runId, content);
        live.userText = content;
        live.userAttachments = sentAttachments;
        ensureLiveUser(live, content);
        showTypingIndicator(live);
        elements.composerInput.value = "";
        committedComposerAttachments();
        resizeComposer();
        updateRuntimeUsage();
        updateConversationChrome();
        renderSessionList();
      }
    } catch (error) {
      if (!queueing) state.pendingSubmission = null;
      showInlineError(error.status === 409
        ? "回复状态刚刚发生变化，正在同步"
        : error.message);
      showToast(error.status === 409 ? "回复状态已同步，请重新发送" : error.message, "error");
      if (error.status === 409) {
        if (sessionId) await loadSessionView(sessionId, { quiet: true });
        else await loadBootstrap();
      }
    } finally {
      state.submitting = false;
      updateControlState();
    }
  }

  function hasHistory() {
    for (const live of state.liveRuns.values()) {
      if (live.userRendered) return true;
    }
    return state.turns.length > 0 || Boolean(elements.timeline.querySelector(".user-message"));
  }

  function openResetDialog() {
    if (typeof elements.resetDialog.showModal === "function") elements.resetDialog.showModal();
    else elements.resetDialog.setAttribute("open", "");
    window.requestAnimationFrame(() => elements.resetCancelButton.focus());
  }

  function openModeChooser() {
    if (state.modeChooserOpen) return;
    state.modeChooserOpen = true;
    updateControlState();
    const overlay = document.createElement("div");
    overlay.className = "mode-chooser-overlay";
    overlay.id = "modeChooserOverlay";
    const panel = document.createElement("div");
    panel.className = "mode-chooser";
    panel.setAttribute("role", "dialog");
    panel.setAttribute("aria-label", "选择新会话模式");
    const title = document.createElement("strong");
    title.textContent = "新会话";
    const hint = document.createElement("small");
    hint.textContent = "选择模式后开始对话；会话模式创建后不可更改";
    panel.append(title, hint);
    const options = [
      { id: "normal", label: "普通模式", icon: "message-circle", desc: "人格、记忆、全部工具" },
      { id: "dev", label: "开发模式", icon: "code", desc: "极简提示词与编码工具，记忆独立" }
    ];
    for (const option of options) {
      const button = document.createElement("button");
      button.type = "button";
      button.className = "mode-chooser-option";
      button.dataset.mode = option.id;
      button.appendChild(makeIconSlot(option.icon));
      const copy = document.createElement("span");
      copy.className = "mode-chooser-copy";
      const label = document.createElement("strong");
      label.textContent = option.label;
      const desc = document.createElement("small");
      desc.textContent = option.desc;
      copy.append(label, desc);
      button.appendChild(copy);
      button.addEventListener("click", () => {
        closeModeChooser();
        closeSidebar();
        createSession(option.id);
      });
      panel.appendChild(button);
    }
    overlay.addEventListener("click", (event) => {
      if (event.target === overlay) closeModeChooser();
    });
    overlay.appendChild(panel);
    document.body.appendChild(overlay);
    const onKey = (event) => {
      if (event.key === "Escape") {
        event.preventDefault();
        closeModeChooser();
      }
    };
    state.modeChooserKeyHandler = onKey;
    document.addEventListener("keydown", onKey, true);
    window.requestAnimationFrame(() => panel.querySelector("button")?.focus());
  }

  function closeModeChooser() {
    if (!state.modeChooserOpen) return;
    state.modeChooserOpen = false;
    if (state.modeChooserKeyHandler) {
      document.removeEventListener("keydown", state.modeChooserKeyHandler, true);
      state.modeChooserKeyHandler = null;
    }
    document.getElementById("modeChooserOverlay")?.remove();
    updateControlState();
  }

  function activeSessionMode() {
    const session = findSession(state.viewSessionId);
    return session?.mode === "dev" ? "dev" : "normal";
  }

  function updateModeBadge() {
    if (!elements.modeBadge) return;
    const mode = activeSessionMode();
    elements.modeBadge.dataset.mode = mode;
    const label = elements.modeBadge.querySelector(".mode-label");
    if (label) label.textContent = mode === "dev" ? "开发" : "普通";
    elements.modeBadge.title = mode === "dev" ? "开发模式会话（创建时定死）" : "普通模式会话（创建时定死）";
  }

  function requestNewConversation() {
    if (multiSessionEnabled()) {
      openModeChooser();
      return;
    }
    closeSidebar();
    if (!hasHistory()) {
      focusComposerIfDesktop();
      return;
    }
    if (conversationRunning() || state.adminBusy || state.submitting) return;
    openResetDialog();
  }

  function requestClearConversation() {
    if (conversationRunning() || state.adminBusy || state.submitting) return;
    if (!hasHistory()) {
      showToast("当前会话没有可清除的记录");
      return;
    }
    openResetDialog();
  }

  async function resetConversation() {
    if (conversationRunning() || state.adminBusy || state.submitting) return;
    state.adminBusy = true;
    elements.resetConfirmButton.disabled = true;
    elements.resetCancelButton.disabled = true;
    elements.resetConfirmButton.textContent = "正在清除";
    updateControlState();
    try {
      if (!state.viewSessionId) throw new Error("无法确定要清除的会话");
      await apiRequest("/api/conversation/reset", {
        method: "POST",
        body: JSON.stringify({ session_id: state.viewSessionId })
      });
      if (elements.resetDialog.open) elements.resetDialog.close("confirmed");
      await loadBootstrap();
      focusComposerIfDesktop();
    } catch (error) {
      showInlineError(error.message);
      showToast(error.message, "error");
      if (error.status === 409) await loadBootstrap();
    } finally {
      state.adminBusy = false;
      elements.resetConfirmButton.disabled = false;
      elements.resetCancelButton.disabled = false;
      elements.resetConfirmButton.textContent = "清空记录";
      updateControlState();
    }
  }

  function handleGlobalKeydown(event) {
    if (elements.settingsDrawer.classList.contains("open") && event.key === "Tab") {
      const focusable = getFocusable(elements.settingsDrawer);
      if (!focusable.length) {
        event.preventDefault();
        elements.settingsDrawer.focus();
        return;
      }
      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      if (event.shiftKey && document.activeElement === first) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first.focus();
      }
    }
    if (event.key === "Escape") {
      if (elements.resetDialog.open) return;
      if (!elements.artifactResourceMenu.hidden) {
        event.preventDefault();
        closeArtifactResourceMenu();
        elements.artifactTitleButton.focus();
        return;
      }
      if (state.sessionMenuFor) {
        event.preventDefault();
        closeSessionMenu();
        return;
      }
      if (!elements.thinkingVariantPopover.hidden) {
        event.preventDefault();
        closeThinkingVariantPopover({ restoreFocus: true });
        return;
      }
      if (!elements.modelMenu.hidden) {
        event.preventDefault();
        closeModelMenu({ restoreFocus: true });
        return;
      }
      if (elements.settingsDrawer.classList.contains("open")) {
        event.preventDefault();
        closeSettings();
        return;
      }
      if (state.artifactOpen) {
        event.preventDefault();
        if (state.artifactMaximized) {
          toggleArtifactMaximized();
          return;
        }
        setArtifactWorkspaceOpen(false);
        elements.artifactToggleButton.focus();
        return;
      }
      if (elements.sidebar.classList.contains("open")) {
        event.preventDefault();
        closeSidebar();
        state.sidebarOpener?.focus?.();
      }
    }
    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "k" && !event.shiftKey && !event.altKey) {
      event.preventDefault();
      requestNewConversation();
    }
  }

  /* ───────────────────────── 控制台 · 数据统计 ─────────────────────────
     数据源:GET /api/usage/stats?range= 与 /api/usage/details。
     图表全部手写 DOM/SVG,与整站同一套 token,离线自包含。 */
  const usageState = {
    range: "1d",
    stats: null,
    loadSeq: 0,
    platformTab: null,
    modelColors: new Map(),
  };
  const USAGE_COLOR_VARS = ["var(--chart-1)", "var(--chart-2)", "var(--chart-4)", "var(--chart-3)"];
  const usageTip = document.createElement("div");
  usageTip.className = "u-chart-tip";
  document.body.appendChild(usageTip);

  function usageTipShow(html, event) {
    usageTip.innerHTML = html;
    usageTip.style.display = "block";
    usageTipMove(event);
  }
  function usageTipMove(event) {
    const width = usageTip.offsetWidth;
    usageTip.style.left = `${Math.min(window.innerWidth - width - 12, event.clientX + 14)}px`;
    usageTip.style.top = `${Math.max(8, event.clientY - usageTip.offsetHeight - 12)}px`;
  }
  function usageTipHide() {
    usageTip.style.display = "none";
  }

  // 计费估算显示:None/0 → null(不渲染);极小值给足小数位。
  function usageFmtCost(usd) {
    if (!Number.isFinite(usd) || usd <= 0) return null;
    if (usd < 0.01) return `$${usd.toFixed(4)}`;
    if (usd < 1) return `$${usd.toFixed(3)}`;
    if (usd < 100) return `$${usd.toFixed(2)}`;
    return `$${usd.toFixed(1)}`;
  }

  function usageFmt(value) {
    if (value >= 1e6) return `${(value / 1e6).toFixed(2)}M`;
    if (value >= 1e3) return `${(value / 1e3).toFixed(1)}k`;
    return String(value);
  }
  function usageSourceName(src) {
    if (src === "agent") return "智能体";
    if (src === "qq" || src === "onebot") return "QQ";
    return src;
  }

  /* ── 图表色派生:跟随当前主题(含 matugen /theme.css 覆盖)──
     取 MD3 三色的"色相",明度错位+色度夹取整形成图表专用色;
     环邻 ΔE<15 时沿明度推开。内置双主题的派生结果已过校验脚本。 */
  const usageSrgbToLinear = (c) => (c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4);
  const usageLinearToSrgb = (c) => (c <= 0.0031308 ? c * 12.92 : 1.055 * c ** (1 / 2.4) - 0.055);
  function usageHexToOklch(hex) {
    const match = /^#?([0-9a-f]{6})$/i.exec(hex.trim());
    if (!match) return null;
    const n = parseInt(match[1], 16);
    const [r, g, b] = [(n >> 16) & 255, (n >> 8) & 255, n & 255].map((v) => usageSrgbToLinear(v / 255));
    const l = Math.cbrt(0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b);
    const m = Math.cbrt(0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b);
    const s = Math.cbrt(0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b);
    const L = 0.2104542553 * l + 0.793617785 * m - 0.0040720468 * s;
    const a = 1.9779984951 * l - 2.428592205 * m + 0.4505937099 * s;
    const bb = 0.0259040371 * l + 0.7827717662 * m - 0.808675766 * s;
    return { L, C: Math.hypot(a, bb), H: (Math.atan2(bb, a) * 180) / Math.PI };
  }
  function usageOklchToHex({ L, C, H }) {
    for (let c = C; c >= 0; c -= 0.004) {
      const h = (H * Math.PI) / 180;
      const a = c * Math.cos(h), bb = c * Math.sin(h);
      const l3 = L + 0.3963377774 * a + 0.2158037573 * bb;
      const m3 = L - 0.1055613458 * a - 0.0638541728 * bb;
      const s3 = L - 0.0894841775 * a - 1.291485548 * bb;
      const [l, m, s] = [l3 ** 3, m3 ** 3, s3 ** 3];
      const r = 4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s;
      const g = -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s;
      const b = -0.0041960863 * l - 0.7034186147 * m + 1.707614701 * s;
      if ([r, g, b].every((v) => v >= -1e-4 && v <= 1 + 1e-4)) {
        const to255 = (v) => Math.round(Math.min(1, Math.max(0, usageLinearToSrgb(v))) * 255);
        return `#${[r, g, b].map((v) => to255(v).toString(16).padStart(2, "0")).join("")}`;
      }
    }
    return "#808080";
  }
  function usageOklabDelta(a, b) {
    const rad = (H) => (H * Math.PI) / 180;
    const [aa, ab] = [a.C * Math.cos(rad(a.H)), a.C * Math.sin(rad(a.H))];
    const [ba, bb] = [b.C * Math.cos(rad(b.H)), b.C * Math.sin(rad(b.H))];
    return Math.hypot(a.L - b.L, aa - ba, ab - bb) * 100;
  }
  function updateChartColors() {
    const styles = getComputedStyle(document.body);
    const read = (name) => usageHexToOklch(styles.getPropertyValue(name));
    const primary = read("--md-sys-color-primary");
    const secondary = read("--md-sys-color-secondary");
    const tertiary = read("--md-sys-color-tertiary");
    const surface = read("--md-sys-color-surface");
    if (!primary || !secondary || !tertiary || !surface) return; // 保底用 CSS 静态色
    const dark = surface.L < 0.5;
    const targetL = dark
      ? { c1: 0.60, c2: 0.66, c3: 0.55, c4: 0.64 }
      : { c1: 0.52, c2: 0.56, c3: 0.47, c4: 0.44 };
    const band = dark ? [0.49, 0.67] : [0.43, 0.62];
    const clampC = (c) => Math.min(0.17, Math.max(0.11, c));
    const colors = {
      c1: { L: targetL.c1, C: clampC(primary.C), H: primary.H },
      c2: { L: targetL.c2, C: clampC(secondary.C), H: secondary.H },
      c3: { L: targetL.c3, C: clampC(tertiary.C), H: tertiary.H },
      c4: { L: targetL.c4, C: clampC(primary.C), H: primary.H - 50 },
    };
    const ring = [["c1", "c2"], ["c2", "c4"], ["c4", "c3"], ["c3", "c1"]];
    for (let pass = 0; pass < 8; pass += 1) {
      let adjusted = false;
      for (const [xa, xb] of ring) {
        if (usageOklabDelta(colors[xa], colors[xb]) < 15) {
          const [lo, hi] = colors[xa].L <= colors[xb].L ? [xa, xb] : [xb, xa];
          colors[lo].L = Math.max(band[0], colors[lo].L - 0.025);
          colors[hi].L = Math.min(band[1], colors[hi].L + 0.025);
          adjusted = true;
        }
      }
      if (!adjusted) break;
    }
    ["c1", "c2", "c3", "c4"].forEach((key, index) =>
      document.body.style.setProperty(`--chart-${index + 1}`, usageOklchToHex(colors[key])));
    const top = dark
      ? { L: 0.72, C: Math.min(0.15, clampC(primary.C)) }
      : { L: 0.42, C: Math.min(0.16, clampC(primary.C)) };
    const base = dark
      ? { L: Math.min(0.34, surface.L + 0.06), C: 0.03 }
      : { L: Math.max(0.88, surface.L - 0.06), C: 0.025 };
    for (let i = 0; i < 5; i += 1) {
      const k = i / 4;
      document.body.style.setProperty(`--heat-${i}`, usageOklchToHex({
        L: base.L + (top.L - base.L) * k,
        C: base.C + (top.C - base.C) * k,
        H: primary.H,
      }));
    }
  }
  /* 色键含 provider:同名模型经不同网关(或模型名缺失)也能分色;
     同一 (provider, model) 跨栏目保持同色。 */
  function usageModelColor(provider, model) {
    const key = `${provider || ""}/${model || ""}`;
    if (!usageState.modelColors.has(key)) {
      usageState.modelColors.set(key, USAGE_COLOR_VARS[usageState.modelColors.size % USAGE_COLOR_VARS.length]);
    }
    return usageState.modelColors.get(key);
  }
  function usageCacheRate(cacheRead, prompt) {
    if (!prompt) return null;
    const rate = Math.min(100, (cacheRead / prompt) * 100);
    // 两位小数;逼近满分时(>99.99)直接封顶 100——命中率是这套缓存
    // 工程的成绩单,四舍五入吃掉小数没有冲击力(验收 08-16)。
    if (rate > 99.99) return "100";
    return rate.toFixed(2);
  }

  function consoleOpen() {
    elements.consoleView.hidden = false;
    elements.consoleView.setAttribute("aria-hidden", "false");
    updateChartColors();
    loadUsageStats();
    loadUsageRecords();
  }
  function consoleClose() {
    elements.consoleView.hidden = true;
    elements.consoleView.setAttribute("aria-hidden", "true");
    usageTipHide();
  }
  function consoleIsOpen() {
    return !elements.consoleView.hidden;
  }

  async function loadUsageStats() {
    const seq = ++usageState.loadSeq;
    elements.usageStamp.textContent = "正在载入…";
    try {
      const response = await apiRequest(`/api/usage/stats?range=${usageState.range}`);
      const data = await response.json();
      if (seq !== usageState.loadSeq) return;
      usageState.stats = data.stats;
      renderUsage();
      const now = new Date();
      const pad = (n) => String(n).padStart(2, "0");
      elements.usageStamp.textContent =
        `更新于 ${pad(now.getHours())}:${pad(now.getMinutes())}:${pad(now.getSeconds())}`;
    } catch (error) {
      if (seq !== usageState.loadSeq) return;
      elements.usageStamp.textContent = `载入失败:${error.message || error}`;
    }
  }

  async function loadUsageRecords() {
    try {
      const params = new URLSearchParams({ limit: "50" });
      if (elements.usageSrcFilter.value) params.set("src", elements.usageSrcFilter.value);
      if (elements.usageModelFilter.value) params.set("model", elements.usageModelFilter.value);
      const response = await apiRequest(`/api/usage/details?${params}`);
      const data = await response.json();
      renderUsageRecords(data.records || []);
    } catch (_) {
      elements.usageRecords.innerHTML =
        `<tr class="u-day-row"><td colspan="7">明细载入失败</td></tr>`;
    }
  }

  /* 筛选选项来自"至今"聚合里出现过的来源与模型;保留当前选中值。 */
  function refreshUsageFilters(stats) {
    const sources = stats.sources || [];
    const fill = (select, entries) => {
      const current = select.value;
      const keepFirst = select.options[0];
      select.replaceChildren(keepFirst);
      for (const [value, label] of entries) {
        const option = document.createElement("option");
        option.value = value;
        option.textContent = label;
        select.appendChild(option);
      }
      select.value = entries.some(([value]) => value === current) ? current : "";
    };
    fill(elements.usageSrcFilter, sources.map((source) => [source.src, usageSourceName(source.src)]));
    const models = new Map();
    for (const source of sources) {
      for (const model of source.models || []) {
        if (model.model) models.set(model.model, true);
      }
    }
    fill(elements.usageModelFilter, [...models.keys()].sort().map((model) => [model, model]));
  }

  function renderUsage() {
    const stats = usageState.stats;
    if (!stats) return;
    renderUsageTiles(stats);
    renderUsageHeat(stats.daily || []);
    renderUsageBars(stats);
    renderUsageSources(stats);
    refreshUsageFilters(stats);
  }

  function renderUsageTiles(stats) {
    const totals = stats.totals || {};
    const prev = stats.prev_totals || null;
    const delta = (current, previous) => {
      if (!prev) return "";
      const base = previous || 0;
      if (!base) return "";
      const value = ((current || 0) / base - 1) * 100;
      const dir = value >= 0 ? "up" : "down";
      const sign = value >= 0 ? "+" : "";
      return `<span class="u-tl-right u-delta ${dir}" title="对比上一周期">${sign}${value.toFixed(0)}%</span>`;
    };
    const hit = usageCacheRate(totals.cache_read || 0, totals.prompt || 0);
    const RING_R = 15;
    const RING_C = 2 * Math.PI * RING_R;
    const icon = (path) => `<svg viewBox="0 0 24 24" aria-hidden="true">${path}</svg>`;
    const dailyAvg = usageState.range === "1d"
      ? ""
      : ` · 日均 ${(Number(totals.requests || 0) / rangeDayCount(stats)).toFixed(1)} 次`;
    const costValue = usageFmtCost(totals.cost);
    const costCoverage = Number(totals.costed_requests || 0) < Number(totals.requests || 0)
      ? `估算覆盖 ${Number(totals.costed_requests || 0).toLocaleString()}/${Number(totals.requests || 0).toLocaleString()} 次`
      : "按 models.dev 价格估算";
    elements.usageTiles.innerHTML = `
      <div class="u-tile"><div class="u-tile-label">${icon('<path d="M18 5H7l6 7-6 7h11"/>')}总消耗${delta(totals.total, prev && prev.total)}</div>
        <div class="u-tile-value">${usageFmt(totals.total || 0)}<small>tokens</small></div>
        <div class="u-tile-sub">输入 ${usageFmt(totals.prompt || 0)} · 输出 ${usageFmt(totals.completion || 0)}</div></div>
      <div class="u-tile"><div class="u-tile-label">${icon('<path d="M12 2v20"/><path d="M17 5H9.5a3.5 3.5 0 0 0 0 7h5a3.5 3.5 0 0 1 0 7H6"/>')}总消费${delta(totals.cost, prev && prev.cost)}</div>
        <div class="u-tile-value">${costValue ? `≈${costValue}` : "—"}</div>
        <div class="u-tile-sub">${costValue ? costCoverage : "暂无价格数据"}</div></div>
      <div class="u-tile"><div class="u-tile-label">${icon('<path d="M22 12h-4l-3 8L9 4l-3 8H2"/>')}请求数${delta(totals.requests, prev && prev.requests)}</div>
        <div class="u-tile-value">${Number(totals.requests || 0).toLocaleString()}</div>
        <div class="u-tile-sub">全部请求:对话 + 辅助${dailyAvg}</div></div>
      <div class="u-tile u-tile-flex"><div class="u-tf-main">
        <div class="u-tile-label">${icon('<circle cx="12" cy="12" r="9"/><circle cx="12" cy="12" r="3.5"/>')}缓存命中率</div>
        <div class="u-tile-value">${hit == null ? "—" : `${hit}<small>%</small>`}</div>
        <div class="u-tile-sub">命中 ${usageFmt(totals.cache_read || 0)} / 输入侧 ${usageFmt(totals.prompt || 0)}</div></div>
        <svg class="u-ring" viewBox="0 0 40 40" aria-hidden="true">
          <circle cx="20" cy="20" r="${RING_R}" fill="none" stroke="var(--chart-1)" stroke-opacity=".22" stroke-width="5"/>
          <circle cx="20" cy="20" r="${RING_R}" fill="none" stroke="var(--chart-3)" stroke-width="5" stroke-linecap="round"
            stroke-dasharray="${(((hit || 0) / 100) * RING_C).toFixed(1)} ${RING_C.toFixed(1)}" transform="rotate(-90 20 20)"/></svg></div>`;
  }

  function rangeDayCount(stats) {
    if (usageState.range === "7d") return 7;
    if (usageState.range === "30d") return 30;
    if (usageState.range === "1d") return 1;
    const daily = stats.daily || [];
    const firstActive = daily.findIndex((day) => day.requests > 0);
    return firstActive === -1 ? 1 : Math.max(1, daily.length - firstActive);
  }

  function usageParseDate(key) {
    const [year, month, day] = key.split("-").map(Number);
    return new Date(year, month - 1, day);
  }

  function renderUsageHeat(daily) {
    const wrap = elements.usageHeatmap;
    const monthsEl = elements.usageHeatMonths;
    wrap.innerHTML = "";
    monthsEl.innerHTML = "";
    if (!daily.length) return;
    const max = Math.max(1, ...daily.map((day) => day.total));
    const firstDate = usageParseDate(daily[0].date);
    const lead = (firstDate.getDay() + 6) % 7;
    for (let index = 0; index < lead; index += 1) {
      const cell = document.createElement("i");
      cell.style.visibility = "hidden";
      wrap.appendChild(cell);
    }
    for (const day of daily) {
      const cell = document.createElement("i");
      cell.dataset.l = day.total === 0 ? 0 : Math.min(4, 1 + Math.floor((day.total / max) * 3.99));
      cell.addEventListener("mousemove", (event) => usageTipShow(
        `<b>${day.date}</b>
         <div class="row"><span>tokens</span><em>${usageFmt(day.total)}</em></div>
         <div class="row"><span>请求</span><em>${day.requests}</em></div>${usageFmtCost(day.cost) ? `
         <div class="row"><span>消费</span><em>≈${usageFmtCost(day.cost)}</em></div>` : ""}`, event));
      cell.addEventListener("mouseleave", usageTipHide);
      wrap.appendChild(cell);
    }
    const columns = Math.ceil((lead + daily.length) / 7);
    let previousMonth = -1;
    for (let column = 0; column < columns; column += 1) {
      const index = Math.min(Math.max(0, column * 7 - lead), daily.length - 1);
      const month = usageParseDate(daily[index].date).getMonth();
      if (month !== previousMonth) {
        const label = document.createElement("span");
        label.textContent = `${month + 1}月`;
        label.style.left = `${(column / columns) * 100}%`;
        monthsEl.appendChild(label);
        previousMonth = month;
      }
    }
    const requests = daily.reduce((sum, day) => sum + day.requests, 0);
    const tokens = daily.reduce((sum, day) => sum + day.total, 0);
    elements.usageHeatTotal.textContent =
      `共 ${requests.toLocaleString()} 次调用 · ${usageFmt(tokens)} tokens`;
  }

  function renderUsageBars(stats) {
    const daily = stats.daily || [];
    let slice;
    let weekly = false;
    if (usageState.range === "1d") slice = daily.slice(-2); // 滚动 24h 跨两个日历日
    else if (usageState.range === "7d") slice = daily.slice(-7);
    else if (usageState.range === "30d") slice = daily.slice(-30);
    else {
      weekly = true;
      slice = [];
      for (let week = 0; week < Math.floor(daily.length / 7); week += 1) {
        const chunk = daily.slice(daily.length - (Math.floor(daily.length / 7) - week) * 7,
          daily.length - (Math.floor(daily.length / 7) - week - 1) * 7);
        if (!chunk.length) continue;
        const merged = { date: chunk[0].date, requests: 0, prompt: 0, completion: 0, cache_read: 0, total: 0, cost: 0 };
        for (const day of chunk) {
          merged.requests += day.requests; merged.prompt += day.prompt;
          merged.completion += day.completion; merged.cache_read += day.cache_read;
          merged.total += day.total; merged.cost += day.cost || 0;
        }
        slice.push(merged);
      }
    }
    elements.usageBarsHint.textContent = weekly ? "按周聚合 · 悬停看明细" : "悬停看明细";
    const bars = elements.usageBars;
    const xs = elements.usageBarsX;
    const ys = elements.usageBarsY;
    bars.innerHTML = ""; xs.innerHTML = ""; ys.innerHTML = "";
    const max = Math.max(...slice.map((day) => day.total), 0);
    if (!max) {
      bars.innerHTML = `<div class="u-empty" style="width:100%">该范围内没有调用记录</div>`;
      return;
    }
    const HEIGHT = 200;
    // 自适应刻度:目标 3-5 条网格线。老的固定档位在单日过亿 token 时
    // 会摆出上百条虚线和重叠标签(条纹背景 bug)。
    const rawStep = max / 4;
    const stepPow = 10 ** Math.floor(Math.log10(Math.max(1, rawStep)));
    const stepUnit = rawStep / stepPow;
    const step = (stepUnit <= 1 ? 1 : stepUnit <= 2 ? 2 : stepUnit <= 5 ? 5 : 10) * stepPow;
    const yLabel = (value, text) => {
      const label = document.createElement("span");
      label.textContent = text;
      label.style.bottom = `${(value / max) * HEIGHT}px`;
      ys.appendChild(label);
    };
    yLabel(0, "0");
    for (let value = step; value <= max; value += step) {
      const grid = document.createElement("div");
      grid.className = "u-gridline";
      grid.style.bottom = `${(value / max) * HEIGHT}px`;
      bars.appendChild(grid);
      yLabel(value, usageFmt(value));
    }
    slice.forEach((day, index) => {
      const slot = document.createElement("div");
      slot.className = "u-bar-slot";
      const column = document.createElement("div");
      column.className = "u-bar-col";
      const fresh = Math.max(0, day.prompt - day.cache_read);
      for (const [value, cls] of [[fresh, "s1"], [day.completion, "s2"], [day.cache_read, "s3"]]) {
        const segment = document.createElement("i");
        segment.className = cls;
        segment.style.height = `${Math.max(value > 0 ? 1 : 0, (value / max) * HEIGHT)}px`;
        column.appendChild(segment);
      }
      slot.appendChild(column);
      column.addEventListener("mousemove", (event) => usageTipShow(
        `<b>${day.date.slice(5)}${weekly ? " 起当周" : ""}</b>
         <div class="row"><span><i style="background:var(--chart-1)"></i>新输入</span><em>${usageFmt(fresh)}</em></div>
         <div class="row"><span><i style="background:var(--chart-2)"></i>输出</span><em>${usageFmt(day.completion)}</em></div>
         <div class="row"><span><i style="background:var(--chart-3)"></i>缓存命中</span><em>${usageFmt(day.cache_read)}</em></div>
         <div class="row"><span>请求</span><em>${day.requests}</em></div>
         <div class="row"><span>合计</span><em>${usageFmt(day.total)}</em></div>${usageFmtCost(day.cost) ? `
         <div class="row"><span>消费</span><em>≈${usageFmtCost(day.cost)}</em></div>` : ""}`, event));
      column.addEventListener("mouseleave", usageTipHide);
      bars.appendChild(slot);
      const label = document.createElement("span");
      label.textContent = weekly
        ? (index % 4 ? "" : day.date.slice(5))
        : slice.length > 16
          ? (index % 5 ? "" : day.date.slice(5))
          : slice.length === 1 ? day.date.slice(5) : day.date.slice(8);
      xs.appendChild(label);
    });
  }

  function renderUsageSources(stats) {
    const container = elements.usageSources;
    container.innerHTML = "";
    const sources = stats.sources || [];
    if (!sources.length) {
      container.innerHTML = `<div class="u-card"><div class="u-empty">该范围内没有调用记录</div></div>`;
      return;
    }
    const agent = sources.find((source) => source.src === "agent");
    const platforms = sources.filter((source) => source.src !== "agent");
    if (agent) {
      container.appendChild(buildUsageSourceCard(
        "模型消耗明细 · 智能体",
        "终端 / WebUI / 定时任务 / 子代理 · 悬停环形图或表行看联动",
        agent,
        stats,
        null,
      ));
    }
    if (platforms.length) {
      if (!usageState.platformTab || !platforms.some((source) => source.src === usageState.platformTab)) {
        usageState.platformTab = platforms[0].src;
      }
      const active = platforms.find((source) => source.src === usageState.platformTab) || platforms[0];
      container.appendChild(buildUsageSourceCard(
        "模型消耗明细 · 通讯平台",
        "按平台分页 · 同一模型全页同色",
        active,
        stats,
        platforms,
      ));
    }
  }

  function buildUsageSourceCard(title, hint, source, stats, platformTabs) {
    const card = document.createElement("div");
    card.className = "u-card";
    const head = document.createElement("div");
    head.className = "u-card-head";
    head.innerHTML = `<h3>${title}</h3><span class="u-hint">${hint}</span>`;
    if (platformTabs && platformTabs.length) {
      const seg = document.createElement("div");
      seg.className = "con-segmented";
      seg.style.marginLeft = "auto";
      for (const platform of platformTabs) {
        const button = document.createElement("button");
        button.type = "button";
        button.textContent = usageSourceName(platform.src);
        button.classList.toggle("on", platform.src === source.src);
        button.addEventListener("click", () => {
          usageState.platformTab = platform.src;
          renderUsageSources(usageState.stats);
        });
        seg.appendChild(button);
      }
      head.appendChild(seg);
    }
    card.appendChild(head);

    const body = document.createElement("div");
    body.className = "u-model-body";
    const donutWrap = document.createElement("div");
    donutWrap.className = "u-donut-wrap";
    const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
    svg.setAttribute("viewBox", "0 0 120 120");
    const center = document.createElement("div");
    center.className = "u-donut-center";
    donutWrap.appendChild(svg);
    donutWrap.appendChild(center);
    body.appendChild(donutWrap);

    const scroll = document.createElement("div");
    scroll.className = "u-table-scroll";
    const table = document.createElement("table");
    table.className = "u-table u-models-table";
    table.innerHTML = `<thead><tr><th>模型</th><th class="num">占比</th><th class="num">请求</th>
      <th class="num">输入</th><th class="num">输出</th><th class="num">消费</th><th>缓存命中</th></tr></thead>`;
    const tbody = document.createElement("tbody");
    const tfoot = document.createElement("tfoot");
    table.appendChild(tbody);
    table.appendChild(tfoot);
    scroll.appendChild(table);
    body.appendChild(scroll);
    card.appendChild(body);

    const aggregate = source;
    const models = source.models || [];
    const requests = Number(aggregate.requests || 0);
    const defCenter = `<div><b>${requests.toLocaleString()}</b><small>次请求</small></div>`;
    center.innerHTML = defCenter;
    if (!models.length || !aggregate.total) {
      tbody.innerHTML = `<tr><td colspan="7"><div class="u-empty">暂无记录</div></td></tr>`;
      return card;
    }

    const globalShare = stats.totals && stats.totals.total
      ? Math.round((aggregate.total / stats.totals.total) * 100)
      : null;
    const sourceHit = usageCacheRate(aggregate.cache_read || 0, aggregate.prompt || 0);
    tfoot.innerHTML = `<tr><td>合计${globalShare == null ? "" :
      ` <small style="color:var(--text-faint);font-weight:400">占全局 ${globalShare}%</small>`}</td>
      <td></td><td class="num">${requests}</td>
      <td class="num">${usageFmt(aggregate.prompt || 0)}</td>
      <td class="num">${usageFmt(aggregate.completion || 0)}</td>
      <td class="num">${usageFmtCost(aggregate.cost) ? `≈${usageFmtCost(aggregate.cost)}` : "—"}</td>
      <td>${sourceHit == null ? "" : `<span class="u-cache-pill">${sourceHit}%</span>`}</td></tr>`;

    const RADIUS = 44;
    const CIRCUM = 2 * Math.PI * RADIUS;
    // 单段就是完整圆环;分段间隙只在真的有多段时存在,且不超过最小段
    // 的一半,防止小切片被间隙吃掉。
    const minShare = Math.min(...models.map((model) => model.total / aggregate.total));
    const GAP = models.length > 1 ? Math.min(3, Math.max(0.5, (minShare * CIRCUM) / 2)) : 0;
    let accumulated = 0;
    models.forEach((model, index) => {
      const share = model.total / aggregate.total;
      const modelName = model.model || "(未标模型)";
      const color = usageModelColor(model.provider, model.model);
      const hit = usageCacheRate(model.cache_read || 0, model.prompt || 0);
      const row = document.createElement("tr");
      row.innerHTML = `<td class="u-model-name"><b><i class="u-dot" style="background:${color}"></i>${modelName}</b>
          <small><i class="u-dot" style="visibility:hidden"></i>${model.provider || "—"}</small></td>
        <td class="num">${Math.round(share * 100)}%</td>
        <td class="num">${model.requests}</td>
        <td class="num">${usageFmt(model.prompt || 0)}</td>
        <td class="num">${usageFmt(model.completion || 0)}</td>
        <td class="num">${usageFmtCost(model.cost) ? `≈${usageFmtCost(model.cost)}` : "—"}</td>
        <td>${hit == null ? "—" : `<span class="u-cache-pill">${hit}%</span>`}</td>`;
      tbody.appendChild(row);

      const length = Math.max(0.5, share * CIRCUM - GAP);
      const circle = document.createElementNS("http://www.w3.org/2000/svg", "circle");
      circle.setAttribute("cx", "60");
      circle.setAttribute("cy", "60");
      circle.setAttribute("r", String(RADIUS));
      circle.setAttribute("fill", "none");
      circle.setAttribute("stroke", color);
      circle.setAttribute("stroke-width", "18");
      circle.setAttribute("stroke-dasharray", `${length.toFixed(2)} ${(CIRCUM - length).toFixed(2)}`);
      circle.setAttribute("stroke-dashoffset", `${(-(accumulated * CIRCUM + GAP / 2)).toFixed(2)}`);
      circle.setAttribute("transform", "rotate(-90 60 60)");
      circle.addEventListener("mousemove", (event) => {
        circle.setAttribute("stroke-width", "21");
        row.classList.add("hl");
        center.innerHTML = `<div><b>${Math.round(share * 100)}%</b><small>${modelName}</small></div>`;
        usageTipShow(
          `<b>${modelName}</b>
           <div class="row"><span>占比</span><em>${Math.round(share * 100)}%</em></div>
           <div class="row"><span>请求</span><em>${model.requests}</em></div>
           <div class="row"><span>输入</span><em>${usageFmt(model.prompt || 0)}</em></div>
           <div class="row"><span>输出</span><em>${usageFmt(model.completion || 0)}</em></div>${usageFmtCost(model.cost) ? `
           <div class="row"><span>消费</span><em>≈${usageFmtCost(model.cost)}</em></div>` : ""}
           <div class="row"><span>缓存命中</span><em>${hit == null ? "—" : `${hit}%`}</em></div>`, event);
      });
      circle.addEventListener("mouseleave", () => {
        circle.setAttribute("stroke-width", "18");
        row.classList.remove("hl");
        center.innerHTML = defCenter;
        usageTipHide();
      });
      row.addEventListener("mouseenter", () => circle.setAttribute("stroke-width", "21"));
      row.addEventListener("mouseleave", () => circle.setAttribute("stroke-width", "18"));
      svg.appendChild(circle);
      accumulated += share;
    });
    return card;
  }

  function renderUsageRecords(records) {
    const tbody = elements.usageRecords;
    tbody.innerHTML = "";
    if (!records.length) {
      tbody.innerHTML = `<tr class="u-day-row"><td colspan="8">还没有任何调用记录</td></tr>`;
      return;
    }
    const today = new Date();
    const dayKey = (date) =>
      `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, "0")}-${String(date.getDate()).padStart(2, "0")}`;
    const todayKey = dayKey(today);
    const yesterday = new Date(today);
    yesterday.setDate(today.getDate() - 1);
    const yesterdayKey = dayKey(yesterday);
    let currentDay = null;
    for (const record of records) {
      const date = new Date(record.ts * 1000);
      const key = dayKey(date);
      if (key !== currentDay) {
        currentDay = key;
        const label = key === todayKey ? "今天" : key === yesterdayKey ? "昨天" : "";
        const row = document.createElement("tr");
        row.className = "u-day-row";
        row.innerHTML = `<td colspan="8">${label ? `${label} · ` : ""}${key.slice(5)}</td>`;
        tbody.appendChild(row);
      }
      const pad = (n) => String(n).padStart(2, "0");
      const hit = usageCacheRate(record.cache_read || 0, record.prompt || 0);
      const row = document.createElement("tr");
      row.innerHTML = `<td class="time">${pad(date.getHours())}:${pad(date.getMinutes())}</td>
        <td><span class="u-src-pill">${usageSourceName(record.src || "agent")}</span></td>
        <td class="u-model-name"><b>${record.model || "(未标模型)"}</b><small>${record.provider || "—"}</small></td>
        <td class="num">${usageFmt(record.prompt || 0)}</td>
        <td class="num">${usageFmt(record.completion || 0)}</td>
        <td class="num">${usageFmtCost(record.cost) ? `≈${usageFmtCost(record.cost)}` : "—"}</td>
        <td>${hit == null ? "—" : `<span class="u-cache-pill">${hit}%</span>`}</td>
        <td><span class="u-type-pill ${record.aux ? "t-aux" : "t-chat"}">${record.aux ? "辅助" : "对话"}</span></td>`;
      tbody.appendChild(row);
    }
  }

  function bindConsoleEvents() {
    elements.consoleButton.addEventListener("click", () => consoleOpen());
    elements.consoleBack.addEventListener("click", () => consoleClose());
    elements.conRailToggle.addEventListener("click", () =>
      elements.consoleView.classList.toggle("rail-collapsed"));
    elements.usageRangeSeg.addEventListener("click", (event) => {
      const button = event.target.closest("button");
      if (!button) return;
      elements.usageRangeSeg.querySelectorAll("button").forEach((other) =>
        other.classList.toggle("on", other === button));
      usageState.range = button.dataset.range;
      loadUsageStats();
    });
    elements.usageRefresh.addEventListener("click", () => {
      updateChartColors();
      loadUsageStats();
      loadUsageRecords();
    });
    elements.usageSrcFilter.addEventListener("change", () => loadUsageRecords());
    elements.usageModelFilter.addEventListener("change", () => loadUsageRecords());
    document.addEventListener("keydown", (event) => {
      if (event.key !== "Escape") return;
      const fullscreenVideo = document.querySelector(".video-shell.webfs");
      if (fullscreenVideo) {
        fullscreenVideo.classList.remove("webfs");
        return;
      }
      if (consoleIsOpen()) consoleClose();
    });
  }

  function bindEvents() {
    bindConsoleEvents();
    elements.mobileMenuButton.addEventListener("click", (event) => openSidebar(event.currentTarget));
    elements.sidebarClose.addEventListener("click", closeSidebar);
    elements.sidebarScrim.addEventListener("click", closeSidebar);
    elements.sidebarCollapseButton?.addEventListener("click", () => setSidebarCollapsed(true));
    elements.sidebarExpandButton?.addEventListener("click", () => setSidebarCollapsed(false));
    elements.topbarSettingsButton.addEventListener("click", (event) => openSettings(event.currentTarget));
    elements.artifactToggleButton.addEventListener("click", () => setArtifactWorkspaceOpen(!state.artifactOpen));
    elements.artifactCloseButton.addEventListener("click", () => setArtifactWorkspaceOpen(false));
    elements.artifactPreviewButton.addEventListener("click", () => setArtifactMode("preview"));
    elements.artifactSourceButton.addEventListener("click", () => setArtifactMode("source"));
    elements.artifactImageZoomOutButton.addEventListener("click", () => changeArtifactImageZoom(-0.25));
    elements.artifactImageZoomInButton.addEventListener("click", () => changeArtifactImageZoom(0.25));
    elements.artifactCopyButton.addEventListener("click", copySelectedArtifact);
    elements.artifactMaximizeButton.addEventListener("click", toggleArtifactMaximized);
    elements.artifactTitleButton.addEventListener("click", (event) => {
      event.stopPropagation();
      if (elements.artifactTitleButton.disabled) return;
      const opening = elements.artifactResourceMenu.hidden;
      elements.artifactResourceMenu.hidden = !opening;
      elements.artifactTitleButton.setAttribute("aria-expanded", String(opening));
    });
    elements.artifactResizeHandle.addEventListener("pointerdown", (event) => {
      if (layoutViewportWidth() <= 760 || state.artifactMaximized) return;
      event.preventDefault();
      elements.artifactResizeHandle.setPointerCapture(event.pointerId);
      const startX = event.clientX;
      const startWidth = elements.artifactWorkspace.offsetWidth;
      let resizeFrame = null;
      let nextRatio = state.artifactWidthRatio;
      const applyResize = () => {
        resizeFrame = null;
        state.artifactWidthRatio = nextRatio;
        syncArtifactLayout();
      };
      const move = (moveEvent) => {
        const viewportWidth = Math.max(320, layoutViewportWidth());
        const pointerDelta = visualPixelsToLayout(startX - moveEvent.clientX);
        const width = Math.min(viewportWidth - 20, Math.max(320, startWidth + pointerDelta));
        nextRatio = width / viewportWidth;
        if (!resizeFrame) resizeFrame = window.requestAnimationFrame(applyResize);
      };
      const finish = () => {
        if (resizeFrame) {
          window.cancelAnimationFrame(resizeFrame);
          applyResize();
        }
        safeStorageSet("miyu.web.artifactWidthRatio.v2", String(state.artifactWidthRatio));
        elements.artifactResizeHandle.removeEventListener("pointermove", move);
        elements.artifactResizeHandle.removeEventListener("pointerup", finish);
        elements.artifactResizeHandle.removeEventListener("pointercancel", finish);
      };
      elements.artifactResizeHandle.addEventListener("pointermove", move);
      elements.artifactResizeHandle.addEventListener("pointerup", finish);
      elements.artifactResizeHandle.addEventListener("pointercancel", finish);
    });
    elements.settingsClose.addEventListener("click", () => closeSettings());
    elements.drawerScrim.addEventListener("click", () => closeSettings());
    elements.settingsNav.querySelectorAll("[data-settings-view]").forEach((button) => {
      button.addEventListener("click", () => setSettingsView(button.dataset.settingsView));
    });
    elements.qqHistoryForm.addEventListener("submit", (event) => {
      event.preventDefault();
      loadQqHistory();
    });
    elements.addProviderButton.addEventListener("click", () => {
      if (!state.configDraft) return;
      state.configDraft.providers = Array.isArray(state.configDraft.providers) ? state.configDraft.providers : [];
      state.configDraft.providers.push(ensureProviderDefaults());
      state.providerSecretStates.push(false);
      refreshProviderSecretStates();
      markConfigDirty();
      renderConfigEditors();
      setSettingsView("providers");
      const cards = elements.providerEditor.querySelectorAll(".provider-card");
      const card = cards[cards.length - 1];
      if (card) {
        card.open = true;
        card.scrollIntoView({ block: "nearest" });
      }
    });
    elements.reloadConfigButton.addEventListener("click", loadConfigDraft);
    elements.saveConfigButton.addEventListener("click", saveConfigDraft);
    elements.applyAdvancedConfigButton.addEventListener("click", applyAdvancedConfig);
    elements.themeButton.addEventListener("click", () => setTheme(elements.body.dataset.theme === "graphite" ? "linen" : "graphite"));
    elements.sidebarThemeButton.addEventListener("click", () => setTheme(elements.body.dataset.theme === "graphite" ? "linen" : "graphite"));
    document.querySelectorAll("[data-theme-choice]").forEach((button) => button.addEventListener("click", () => setTheme(button.dataset.themeChoice)));
    document.querySelectorAll("[data-scheme-choice]").forEach((button) => button.addEventListener("click", () => setColorScheme(button.dataset.schemeChoice)));
    document.querySelectorAll("[data-chat-font]").forEach((button) => button.addEventListener("click", () => setChatFontSize(button.dataset.chatFont)));
    elements.reasoningExpandToggle?.addEventListener("click", () => setReasoningExpanded(!state.reasoningExpanded));
    elements.toolExpandToggle?.addEventListener("click", () => setToolExpanded(!state.toolExpanded));
    elements.thinkingVariantButton.addEventListener("click", () => {
      if (elements.thinkingVariantPopover.hidden) openThinkingVariantPopover();
      else closeThinkingVariantPopover({ restoreFocus: true });
    });
    elements.thinkingVariantPopover.addEventListener("keydown", handleThinkingVariantKeydown);
    elements.modelButton.addEventListener("click", (event) => {
      event.stopPropagation();
      if (elements.modelMenu.hidden) openModelMenu();
      else closeModelMenu({ restoreFocus: true });
    });
    elements.modelMenu.addEventListener("keydown", (event) => {
      const items = Array.from(elements.modelMenu.querySelectorAll("button:not(:disabled)"));
      const index = items.indexOf(document.activeElement);
      if (event.key === "ArrowDown" || event.key === "ArrowUp") {
        event.preventDefault();
        const direction = event.key === "ArrowDown" ? 1 : -1;
        items[(index + direction + items.length) % items.length]?.focus();
      } else if (event.key === "Home" || event.key === "End") {
        event.preventDefault();
        items[event.key === "Home" ? 0 : items.length - 1]?.focus();
      } else if (event.key === "Escape") {
        event.preventDefault();
        closeModelMenu({ restoreFocus: true });
      }
    });
    document.addEventListener("pointerdown", (event) => {
      if (!elements.thinkingVariantPopover.hidden
        && !event.target.closest("#thinkingVariantPopover")
        && !event.target.closest("#thinkingVariantButton")) {
        closeThinkingVariantPopover();
      }
    });
    document.addEventListener("click", (event) => {
      if (!elements.modelMenu.hidden && !event.target.closest("#modelMenuWrap")) closeModelMenu();
      if (state.sessionMenuFor && !event.target.closest(".session-menu") && !event.target.closest(".session-menu-button")) closeSessionMenu();
      if (!elements.artifactResourceMenu.hidden && !event.target.closest(".artifact-resource-wrap")) closeArtifactResourceMenu();
    });
    elements.promptGrid.querySelectorAll("[data-prompt]").forEach((button) => {
      button.addEventListener("click", () => {
        if (elements.composerInput.disabled) return;
        elements.composerInput.value = button.dataset.prompt || "";
        resizeComposer();
        elements.composerInput.focus();
      });
    });
    elements.composerInput.addEventListener("input", resizeComposer);
    elements.attachButton.addEventListener("click", () => elements.attachmentInput.click());
    elements.attachmentInput.addEventListener("change", () => {
      addComposerFiles(elements.attachmentInput.files);
      elements.attachmentInput.value = "";
    });
    elements.composerForm.addEventListener("dragenter", (event) => {
      if (!event.dataTransfer?.types?.includes("Files")) return;
      event.preventDefault();
      elements.composerForm.classList.add("is-dragging");
    });
    elements.composerForm.addEventListener("dragover", (event) => {
      if (!event.dataTransfer?.types?.includes("Files")) return;
      event.preventDefault();
      event.dataTransfer.dropEffect = "copy";
      elements.composerForm.classList.add("is-dragging");
    });
    elements.composerForm.addEventListener("dragleave", (event) => {
      if (!elements.composerForm.contains(event.relatedTarget)) elements.composerForm.classList.remove("is-dragging");
    });
    elements.composerForm.addEventListener("drop", (event) => {
      elements.composerForm.classList.remove("is-dragging");
      const files = collectTransferFiles(event.dataTransfer);
      if (!files.length) return;
      event.preventDefault();
      addComposerFiles(files);
    });
    elements.composerInput.addEventListener("paste", (event) => {
      const files = collectTransferFiles(event.clipboardData);
      if (!files.length) {
        const hasUriList = Array.from(event.clipboardData?.items || []).some((item) => item.type === "text/uri-list");
        if (hasUriList) showToast("浏览器没有提供文件内容，请直接拖入输入框", "error");
        return;
      }
      event.preventDefault();
      addComposerFiles(files);
    });
    elements.composerInput.addEventListener("compositionstart", () => {
      state.composing = true;
    });
    elements.composerInput.addEventListener("compositionend", () => {
      state.composing = false;
    });
    elements.composerInput.addEventListener("keydown", (event) => {
      if (event.key === "Enter" && !event.shiftKey && !event.isComposing && !state.composing && event.keyCode !== 229) {
        event.preventDefault();
        if (!elements.sendButton.disabled) elements.composerForm.requestSubmit();
      }
    });
    elements.composerForm.addEventListener("submit", (event) => {
      event.preventDefault();
      submitTurn();
    });
    elements.loginForm.addEventListener("submit", (event) => {
      event.preventDefault();
      submitLogin();
    });
    elements.newChatButton.addEventListener("click", requestNewConversation);
    elements.retryBootstrapButton.addEventListener("click", loadBootstrap);
    elements.resetConfirmButton.addEventListener("click", resetConversation);
    elements.chatScroll.addEventListener("scroll", () => {
      state.nearBottom = isNearBottom();
      if (state.programmaticScroll) return;
      if (!state.followOutput && isAtBottom()) {
        state.followOutput = true;
        elements.jumpBottomButton.hidden = true;
      } else if (!state.followOutput || !state.nearBottom) {
        suspendOutputFollowing();
      }
    }, { passive: true });
    elements.chatScroll.addEventListener("wheel", (event) => {
      if (event.deltaY < 0) suspendOutputFollowing();
    }, { passive: true });
    elements.chatScroll.addEventListener("touchmove", () => {
      suspendOutputFollowing();
    }, { passive: true });
    elements.jumpBottomButton.addEventListener("click", () => scrollToBottom({ force: true, smooth: true }));
    window.addEventListener("resize", () => {
      updateJumpButtonOffset();
      syncArtifactLayout();
      positionThinkingVariantPopover();
    }, { passive: true });
    new ResizeObserver(syncArtifactLayout).observe(elements.mainStage);
    if (window.visualViewport) {
      window.visualViewport.addEventListener("resize", syncAppHeight, { passive: true });
      window.visualViewport.addEventListener("resize", positionThinkingVariantPopover, { passive: true });
      window.visualViewport.addEventListener("scroll", positionThinkingVariantPopover, { passive: true });
      syncAppHeight();
    }
    document.addEventListener("keydown", handleGlobalKeydown);
  }

  function syncAppHeight() {
    const viewport = window.visualViewport;
    if (!viewport) return;
    document.documentElement.style.setProperty("--app-height", `${Math.round(viewport.height * viewport.scale / UI_SCALE)}px`);
  }

  function initialize() {
    renderIconSlots();
    if (window.location.hash.includes("console")) consoleOpen();
    setTheme(safeStorageGet("miyu.web.theme") || "graphite", false);
    const storedScheme = safeStorageGet("miyu.web.colorScheme");
    if (storedScheme) setColorScheme(storedScheme, false);
    probeMatugenTheme();
    setChatFontSize(safeStorageGet("miyu.web.chatFontSize") || "15px", false);
    setReasoningExpanded(safeStorageGet("miyu.web.reasoningExpanded") === "true", false);
    setToolExpanded(safeStorageGet("miyu.web.toolExpanded") === "true", false);
    const artifactRatio = Number(safeStorageGet("miyu.web.artifactWidthRatio.v2"));
    if (Number.isFinite(artifactRatio) && artifactRatio >= 0.25 && artifactRatio <= 0.9) {
      state.artifactWidthRatio = artifactRatio;
    }
    setSidebarCollapsed(safeStorageGet("miyu.web.sidebarCollapsed") === "true");
    syncArtifactLayout();
    setSettingsView("interface");
    bindEvents();
    resizeComposer();
    updateSettingsControls();
    loadBootstrap();
  }

  initialize();
})();
