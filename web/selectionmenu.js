"use strict";

/*
 * 选中文字右键菜单:解释 · 翻译 · 引用追问 · 搜索 · 复制。
 * 设计取舍(2026-09-14):中性说明、知识库与网页两栏、
 * 不留历史、侧栏里的选区先不管)。
 *
 * - 只在聊天消息正文(.markdown-body / .user-bubble)里、选区非空时拦截右键;其他地方与
 *   Shift + 右键一律走浏览器原生菜单。替掉原生菜单不能把退路也拿走,所以菜单自带「复制」。
 * - 解释 / 翻译:POST /api/selection/assist,NDJSON 流式。上下文由后端按 turn_id 取,前端不传。
 * - 搜索:知识库(/api/dash/kb/search)在前、网页(/api/selection/web-search)在后。
 * - 结果只进浮窗:不进对话、不进记忆、不留历史。关浮窗即中断请求。
 * - 手机:长按菜单拦不住也不该拦,选区停稳 300ms 后在选区下方浮一条工具条(上方是系统菜单)。
 *
 * 菜单与浮窗挂 document.body、fixed 定位:聊天区外层有 overflow 裁剪。
 * 单独成文件:app.js 已经上万行(与 contextpanel.js / todos.js 同构)。
 */
window.MiyuSelectionMenu = (() => {
  const ACTIONS = [
    { key: "explain", label: "解释", needsModel: true },
    { key: "translate", label: "翻译", needsModel: true },
    { key: "quote", label: "引用追问" },
    { key: "search", label: "搜索" },
  ];
  const MAX_CHARS = 2000;
  const BODY_SELECTOR = ".markdown-body, .user-bubble";

  let ctx = null;
  let menu = null;
  let toolbar = null;
  let current = null; // { text, turnId, rect }
  const popovers = []; // { node, head, body, foot, controller, pinned }
  let selectionTimer = 0;

  function el(tag, className, text) {
    const node = document.createElement(tag);
    if (className) node.className = className;
    if (text != null) node.textContent = text;
    return node;
  }
  function button(className, text, onClick, title) {
    const node = el("button", className, text);
    node.type = "button";
    if (onClick) node.addEventListener("click", onClick);
    if (title) node.title = title;
    return node;
  }
  const elementOf = (node) => (node?.nodeType === Node.ELEMENT_NODE ? node : node?.parentElement) || null;

  function mount(options) {
    ctx = options;
    if (!ctx?.root) return;
    ctx.root.addEventListener("contextmenu", onContextMenu);
    ctx.root.addEventListener("scroll", closeMenu, { passive: true, capture: true });
    document.addEventListener("pointerdown", onPointerDown, true);
    document.addEventListener("keydown", onKeyDown);
    window.addEventListener("resize", closeMenu, { passive: true });
    if (window.matchMedia("(hover: none), (pointer: coarse)").matches) {
      document.addEventListener("selectionchange", onSelectionChange);
    }
  }

  /// 选区必须整段落在同一条消息的正文里,才算我们的地盘。
  function readSelection() {
    const selection = window.getSelection();
    if (!selection || selection.isCollapsed || !selection.rangeCount) return null;
    const text = selection.toString().trim();
    if (!text) return null;
    const range = selection.getRangeAt(0);
    const start = elementOf(range.startContainer);
    const end = elementOf(range.endContainer);
    if (!start || !end || !ctx.root.contains(start) || !ctx.root.contains(end)) return null;
    const startBody = start.closest(BODY_SELECTOR);
    const endBody = end.closest(BODY_SELECTOR);
    if (!startBody || !endBody) return null;
    const article = start.closest("article.message");
    if (!article || article !== end.closest("article.message")) return null;
    return { text, turnId: article.dataset.turnId || "", rect: range.getBoundingClientRect() };
  }

  function onContextMenu(event) {
    if (event.shiftKey) return;
    const picked = readSelection();
    if (!picked) return;
    event.preventDefault();
    current = picked;
    openMenu(event.clientX, event.clientY);
  }

  function openMenu(x, y) {
    closeMenu();
    menu = el("div", "sel-menu");
    menu.setAttribute("role", "menu");
    const tooLong = current.text.length > MAX_CHARS;
    for (const action of ACTIONS) {
      const item = menuItem(action.label, () => runAction(action.key));
      if (action.needsModel && tooLong) {
        item.disabled = true;
        item.title = `选中的文字超过 ${MAX_CHARS} 字`;
      }
      menu.appendChild(item);
    }
    menu.append(el("div", "sel-menu-sep"), menuItem("复制", copySelection), el("div", "sel-menu-hint", "Shift + 右键:浏览器菜单"));
    // 按下按钮会让部分浏览器收起选区;选区文字已经记在 current 里,这里只防闪。
    menu.addEventListener("pointerdown", (event) => event.preventDefault());
    document.body.appendChild(menu);
    const { width, height } = menu.getBoundingClientRect();
    menu.style.left = `${Math.max(8, Math.min(x, window.innerWidth - width - 8))}px`;
    menu.style.top = `${y + height + 8 > window.innerHeight ? Math.max(8, y - height) : y}px`;
    menu.querySelector(".sel-menu-item:not(:disabled)")?.focus({ preventScroll: true });
  }

  function menuItem(label, onClick) {
    const item = button("sel-menu-item", label, () => {
      closeMenu();
      onClick();
    });
    item.setAttribute("role", "menuitem");
    return item;
  }

  function closeMenu() {
    menu?.remove();
    menu = null;
  }

  function onPointerDown(event) {
    const target = event.target;
    if (menu && !menu.contains(target)) closeMenu();
    if (toolbar && !toolbar.hidden && toolbar.contains(target)) return;
    for (const pop of [...popovers]) {
      if (!pop.pinned && !pop.node.contains(target)) closePopover(pop);
    }
  }

  function onKeyDown(event) {
    if (menu) {
      const items = [...menu.querySelectorAll(".sel-menu-item:not(:disabled)")];
      if (event.key === "ArrowDown" || event.key === "ArrowUp") {
        event.preventDefault();
        const index = items.indexOf(document.activeElement);
        const step = event.key === "ArrowDown" ? 1 : -1;
        items[(index + step + items.length) % items.length]?.focus();
      } else if (event.key === "Escape") {
        event.preventDefault();
        closeMenu();
      }
      return;
    }
    if (event.key !== "Escape" || !popovers.length) return;
    const target = [...popovers].reverse().find((pop) => !pop.pinned) || popovers[popovers.length - 1];
    event.preventDefault();
    closePopover(target);
  }

  function runAction(key) {
    hideToolbar();
    if (!current) return;
    if (key === "quote") quote(current.text);
    else if (key === "search") openSearch(current);
    else openAssist(key, current);
  }

  // ---------------- 引用追问 / 复制 ----------------

  function quote(text, extra = "") {
    const input = ctx.composer;
    if (!input) return;
    const block = text.split("\n").map((line) => `> ${line}`).join("\n");
    const draft = input.value.replace(/\s+$/, "");
    input.value = `${draft ? `${draft}\n\n` : ""}${block}\n${extra ? `\n${extra}\n` : ""}\n`;
    input.dispatchEvent(new Event("input", { bubbles: true }));
    ctx.resizeComposer?.();
    input.focus();
    input.setSelectionRange(input.value.length, input.value.length);
    window.getSelection()?.removeAllRanges();
  }

  async function copyText(text, message = "已复制") {
    try {
      await navigator.clipboard.writeText(text);
      ctx.toast?.(message);
    } catch (_) {
      ctx.toast?.("复制失败:浏览器没给剪贴板权限", "error");
    }
  }

  function copySelection() {
    if (current) copyText(current.text);
  }

  // ---------------- 浮窗 ----------------

  function createPopover(title, picked) {
    // 同一时间只留一个没钉住的浮窗,再开就替换它;钉住的留着。
    for (const pop of [...popovers]) {
      if (!pop.pinned) closePopover(pop);
    }
    const node = el("section", "sel-pop");
    node.setAttribute("role", "dialog");
    node.setAttribute("aria-label", title);
    const head = el("header", "sel-pop-head");
    const excerpt = picked.text.replace(/\s+/g, " ");
    const quoteNode = el("span", "sel-pop-quote", excerpt.length > 60 ? `${excerpt.slice(0, 60)}…` : excerpt);
    quoteNode.title = picked.text;
    const pop = { node, head, body: null, foot: null, controller: null, pinned: false };
    const pin = button("sel-icon", "钉住", () => {
      pop.pinned = !pop.pinned;
      pin.classList.toggle("is-active", pop.pinned);
      pin.textContent = pop.pinned ? "已钉住" : "钉住";
    }, "钉住后点别处不会关,可以再选别的词");
    head.append(el("strong", null, title), quoteNode, pin, button("sel-icon", "✕", () => closePopover(pop), "关闭"));
    pop.body = el("div", "sel-pop-body");
    pop.foot = el("footer", "sel-pop-foot");
    node.append(head, pop.body, pop.foot);
    document.body.appendChild(node);
    popovers.push(pop);
    placePopover(node, picked.rect);
    makeDraggable(pop, head, () => {
      if (!pop.pinned) pin.click();
    });
    return pop;
  }

  /// 按住标题栏拖动浮窗(标题栏里的按钮照常点)。拖过的浮窗自动钉住:挪到一边就是想留着看,
  /// 点别处不该把它关掉。手机宽度下浮窗是底部面板,不拖。
  function makeDraggable(pop, handle, onDragStart) {
    handle.addEventListener("pointerdown", (event) => {
      if (event.button !== 0 || event.target.closest("button")) return;
      if (window.matchMedia("(max-width: 640px)").matches) return;
      event.preventDefault();
      const node = pop.node;
      const rect = node.getBoundingClientRect();
      const offsetX = event.clientX - rect.left;
      const offsetY = event.clientY - rect.top;
      // 原先可能是贴在选区上方(用 bottom 定位),拖动统一换成 top/left。
      const startMax = parseFloat(node.style.maxHeight) || rect.height;
      node.style.bottom = "";
      node.style.top = `${rect.top}px`;
      node.style.left = `${rect.left}px`;
      node.classList.add("is-dragging");
      handle.setPointerCapture(event.pointerId);
      onDragStart?.();
      const move = (moveEvent) => {
        const left = Math.min(Math.max(8, moveEvent.clientX - offsetX), window.innerWidth - rect.width - 8);
        // 标题栏始终留在视口里,拖到底部时内容区收矮而不是整个跑出屏幕。
        const top = Math.min(Math.max(8, moveEvent.clientY - offsetY), window.innerHeight - 48);
        node.style.left = `${left}px`;
        node.style.top = `${top}px`;
        node.style.maxHeight = `${Math.max(48, Math.min(startMax, window.innerHeight - top - 8))}px`;
      };
      const end = () => {
        node.classList.remove("is-dragging");
        handle.removeEventListener("pointermove", move);
        handle.removeEventListener("pointerup", end);
        handle.removeEventListener("pointercancel", end);
      };
      handle.addEventListener("pointermove", move);
      handle.addEventListener("pointerup", end);
      handle.addEventListener("pointercancel", end);
    });
  }

  /// 贴在选区下方,下方放不下且上方更宽就翻到上方;手机宽度由 CSS 改成底部面板。
  function placePopover(node, rect) {
    const gap = 8;
    const width = Math.min(400, window.innerWidth - 16);
    node.style.width = `${width}px`;
    node.style.left = `${Math.min(Math.max(8, rect.left), window.innerWidth - width - 8)}px`;
    const below = window.innerHeight - rect.bottom - gap;
    const above = rect.top - gap;
    if (below >= 240 || below >= above) {
      node.style.top = `${rect.bottom + gap}px`;
      node.style.maxHeight = `${Math.max(160, below - 8)}px`;
    } else {
      node.style.bottom = `${window.innerHeight - rect.top + gap}px`;
      node.style.maxHeight = `${Math.max(160, above - 8)}px`;
    }
  }

  function closePopover(pop) {
    pop.controller?.abort();
    pop.node.remove();
    const index = popovers.indexOf(pop);
    if (index >= 0) popovers.splice(index, 1);
  }

  function statusLine(text) {
    const node = el("div", "sel-status");
    node.append(el("span", "sel-spinner"), document.createTextNode(text));
    return node;
  }

  // ---------------- 解释 / 翻译 ----------------

  /// 汉字占多数就译成英文,否则译成中文;浮窗里可以切。
  function autoTarget(text) {
    const compact = text.replace(/\s+/g, "");
    const cjk = (compact.match(/[㐀-鿿豈-﫿]/g) || []).length;
    return compact && cjk * 2 >= compact.length ? "en" : "zh";
  }

  function openAssist(kind, picked, targetLang) {
    const title = kind === "explain" ? "解释" : "翻译";
    const pop = createPopover(title, picked);
    const lang = kind === "translate" ? targetLang || autoTarget(picked.text) : null;
    if (kind === "translate") {
      const other = lang === "en" ? "zh" : "en";
      pop.head.insertBefore(
        button("sel-chip", other === "en" ? "改译英文" : "改译中文", () => {
          closePopover(pop);
          openAssist("translate", picked, other);
        }),
        pop.head.children[2]
      );
    }
    if (!picked.turnId) pop.body.appendChild(el("div", "sel-note", "这条消息还没落库,这次不带对话上下文。"));
    const status = statusLine(kind === "explain" ? "正在解释…" : "正在翻译…");
    const output = el("div", "sel-output markdown-body");
    pop.body.append(status, output);

    let text = "";
    let frame = 0;
    const paint = () => {
      frame = 0;
      ctx.renderMarkdown(output, text);
    };
    const renderFoot = (done) => {
      pop.foot.replaceChildren();
      const copy = button("sel-btn", "复制", () => copyText(text));
      const retry = button("sel-btn", "重试", () => {
        closePopover(pop);
        openAssist(kind, picked, lang);
      });
      const ask = button("sel-btn", "转成追问", () => {
        quote(picked.text, `${title}:\n${text}`);
        closePopover(pop);
      }, "把选中文字和这段结果一起放进输入框");
      copy.disabled = !done || !text;
      ask.disabled = !done || !text;
      pop.foot.append(copy, retry, ask);
    };
    renderFoot(false);

    pop.controller = new AbortController();
    streamAssist(
      {
        session_id: ctx.getSessionId(),
        turn_id: picked.turnId || null,
        action: kind,
        text: picked.text,
        target_lang: lang,
      },
      pop.controller.signal,
      {
        delta(chunk) {
          text += chunk;
          status.remove();
          if (!frame) frame = window.requestAnimationFrame(paint);
        },
        done(event) {
          if (!text && event?.text) text = String(event.text);
          status.remove();
          if (frame) window.cancelAnimationFrame(frame);
          paint();
          if (!text) output.textContent = "模型没有返回内容";
          renderFoot(true);
        },
        error(message) {
          status.remove();
          pop.body.appendChild(el("div", "sel-note is-error", message));
          renderFoot(Boolean(text));
        },
      }
    );
  }

  async function streamAssist(payload, signal, handlers) {
    let response;
    try {
      response = await ctx.apiRequest("/api/selection/assist", {
        method: "POST",
        body: JSON.stringify(payload),
        signal,
      });
    } catch (error) {
      if (!signal.aborted) handlers.error(error?.message || "请求失败");
      return;
    }
    const reader = response.body?.getReader();
    if (!reader) {
      handlers.error("浏览器不支持流式读取");
      return;
    }
    const decoder = new TextDecoder();
    let buffer = "";
    let finished = false;
    try {
      for (;;) {
        const { value, done } = await reader.read();
        if (done) break;
        buffer += decoder.decode(value, { stream: true });
        let index;
        while ((index = buffer.indexOf("\n")) >= 0) {
          const line = buffer.slice(0, index).trim();
          buffer = buffer.slice(index + 1);
          if (!line) continue;
          let event;
          try {
            event = JSON.parse(line);
          } catch (_) {
            continue;
          }
          if (event.type === "delta") handlers.delta(String(event.text || ""));
          else if (event.type === "done") {
            finished = true;
            handlers.done(event);
          } else if (event.type === "error") {
            finished = true;
            handlers.error(String(event.message || "请求失败"));
          }
        }
      }
    } catch (error) {
      if (!signal.aborted) handlers.error(error?.message || "连接中断");
      return;
    }
    if (!finished && !signal.aborted) handlers.error("连接提前结束,没有收到结果");
  }

  // ---------------- 搜索 ----------------

  function openSearch(picked) {
    const pop = createPopover("搜索", picked);
    const query = picked.text.replace(/\s+/g, " ").slice(0, 200);
    const kb = searchSection(pop.body, "知识库");
    const web = searchSection(pop.body, "网页");
    pop.controller = new AbortController();
    const { signal } = pop.controller;
    ctx.apiRequest(`/api/dash/kb/search?q=${encodeURIComponent(query)}&limit=5`, { signal })
      .then((response) => response.json())
      .then((data) => renderKb(kb, data))
      .catch((error) => {
        if (!signal.aborted) sectionError(kb, error);
      });
    ctx.apiRequest(`/api/selection/web-search?q=${encodeURIComponent(query)}`, { signal })
      .then((response) => response.json())
      .then((data) => renderWeb(web, data))
      .catch((error) => {
        if (!signal.aborted) sectionError(web, error);
      });
    pop.foot.append(
      button("sel-btn", "复制关键词", () => copyText(query)),
      button("sel-btn", "引用追问", () => {
        quote(picked.text);
        closePopover(pop);
      })
    );
  }

  function searchSection(parent, label) {
    const section = el("section", "sel-section");
    const body = el("div", "sel-section-body");
    body.appendChild(statusLine("正在搜索…"));
    section.append(el("div", "sel-section-head", label), body);
    parent.appendChild(section);
    return body;
  }

  function sectionError(body, error) {
    body.replaceChildren(el("div", "sel-note is-error", error?.message || "搜索失败"));
  }

  function renderKb(body, data) {
    body.replaceChildren();
    const results = Array.isArray(data?.results) ? data.results : [];
    if (!results.length) {
      body.appendChild(el("div", "sel-empty", "知识库里没有匹配"));
      return;
    }
    for (const item of results.slice(0, 5)) {
      const path = String(item.path || item.rel_path || item.file || item.name || item.title || "");
      const snippet = String(item.snippet || item.excerpt || item.preview || item.content || item.text || "")
        .replace(/\s+/g, " ")
        .slice(0, 140);
      // 侧栏临时视图(file-links 步 2)建好之前,点一下先复制路径。
      const row = button("sel-result", null, () => copyText(path, "已复制文件路径"), "复制路径");
      row.appendChild(el("span", "sel-result-title", path || "(无路径)"));
      if (snippet) row.appendChild(el("span", "sel-result-snippet", snippet));
      body.appendChild(row);
    }
  }

  function renderWeb(body, data) {
    body.replaceChildren();
    const output = String(data?.output || "").trim();
    if (!output) {
      body.appendChild(el("div", "sel-empty", "网页搜索没有结果"));
      return;
    }
    let parsed = null;
    try {
      parsed = JSON.parse(output);
    } catch (_) {
      parsed = null;
    }
    const results = Array.isArray(parsed?.results) ? parsed.results : null;
    if (!results) {
      const markdown = el("div", "sel-output markdown-body");
      ctx.renderMarkdown(markdown, output);
      body.appendChild(markdown);
      return;
    }
    for (const item of results.slice(0, 6)) {
      const url = String(item.url || item.link || "");
      const title = String(item.title || url || "(无标题)");
      const snippet = String(item.snippet || item.content || item.description || "").replace(/\s+/g, " ").slice(0, 140);
      // 只放行 http(s) 链接:结果来自外部搜索服务,是不可信数据。
      const safe = /^https?:\/\//i.test(url);
      const row = safe ? el("a", "sel-result") : el("div", "sel-result");
      if (safe) {
        row.href = url;
        row.target = "_blank";
        row.rel = "noopener noreferrer";
      }
      row.appendChild(el("span", "sel-result-title", title));
      if (snippet) row.appendChild(el("span", "sel-result-snippet", snippet));
      if (url) row.appendChild(el("span", "sel-result-url", url));
      body.appendChild(row);
    }
  }

  // ---------------- 手机工具条 ----------------

  function onSelectionChange() {
    window.clearTimeout(selectionTimer);
    selectionTimer = window.setTimeout(() => {
      const picked = readSelection();
      if (!picked) {
        hideToolbar();
        return;
      }
      current = picked;
      showToolbar(picked.rect);
    }, 300);
  }

  function showToolbar(rect) {
    if (!toolbar) {
      toolbar = el("div", "sel-toolbar");
      toolbar.setAttribute("role", "toolbar");
      for (const action of ACTIONS) toolbar.appendChild(button("sel-toolbar-item", action.label, () => runAction(action.key)));
      toolbar.appendChild(button("sel-toolbar-item", "复制", () => {
        copySelection();
        hideToolbar();
      }));
      // 点工具条别让系统先把选区收掉。
      toolbar.addEventListener("pointerdown", (event) => event.preventDefault());
      document.body.appendChild(toolbar);
    }
    toolbar.hidden = false;
    const { width, height } = toolbar.getBoundingClientRect();
    const below = rect.bottom + 10;
    toolbar.style.top = `${below + height < window.innerHeight - 8 ? below : Math.max(8, rect.top - height - 10)}px`;
    toolbar.style.left = `${Math.min(Math.max(8, rect.left + rect.width / 2 - width / 2), window.innerWidth - width - 8)}px`;
  }

  function hideToolbar() {
    if (toolbar) toolbar.hidden = true;
  }

  return { mount };
})();
