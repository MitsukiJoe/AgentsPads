"use strict";

(() => {
  const $ = (selector) => document.querySelector(selector);
  const $$ = (selector) => [...document.querySelectorAll(selector)];
  const en = () => document.documentElement.lang === "en";
  const t = (zh, english) => (en() ? english : zh);
  const modes = ["touchpad", "trackball", "trackpoint"];
  const input = $("#remote-input");
  const pad = $("#pointer-area");
  const cursor = $("#demo-cursor");
  let mode = "touchpad";
  let received = "";
  const history = [];
  let status = "ready";
  let shortcut = "";
  let autoEnter = false;
  let voiceAuto = true;
  let inputHeight = 0;
  let pointer = { x: 0.58, y: 0.61 };
  let gesture = null;
  let animation = 0;
  let velocity = { x: 0, y: 0 };
  function paintInput() {
    $("#draft-count").textContent = `${input.value.length} / 500`;
    input.placeholder = t(
      "打字或用语音键盘",
      "Type or use your voice keyboard",
    );
    const empty = !input.value.length;
    $("#send-text").setAttribute(
      "aria-label",
      empty ? t("回车", "Enter") : t("发送文字", "Send text"),
    );
    $("#send-text").title = empty
      ? t("回车", "Enter")
      : t("发送文字", "Send text");
    $("#send-text").innerHTML = empty
      ? '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M20 5v9H4m5-5-5 5 5 5"/></svg>'
      : '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="m3 3 19 9-19 9 4-9-4-9Zm4 9h15"/></svg>';
  }
  function paintStatus() {
    const labels = {
      ready: [
        "演示已就绪，发一句话试试。",
        "Demo ready. Try sending a message.",
      ],
      sent: [
        "文字已投递到电脑的模拟接收区。",
        "Text delivered to the simulated receiver.",
      ],
      key: [`模拟按键：${shortcut}`, `Simulated key: ${shortcut}`],
      pointer: [
        "指针正在电脑的模拟桌面移动。",
        "The pointer is moving on the simulated desktop.",
      ],
      undo: [
        "已撤回这次演示中的上次文字投递。",
        "Undid the last text delivery in this demo.",
      ],
      voice: [
        voiceAuto
          ? "已开启语音自动发送的演示开关；网页不采集语音。"
          : "已关闭语音自动发送的演示开关。",
        voiceAuto
          ? "Voice auto-send demo is on; this page does not capture audio."
          : "Voice auto-send demo is off.",
      ],
      auto: [
        autoEnter
          ? "演示已开启：投递文字后模拟按一次 Enter。"
          : "演示已关闭电脑自动回车。",
        autoEnter
          ? "Demo: delivered text is followed by Enter."
          : "Auto Enter demo is off.",
      ],
    };
    $("#receiver-status").textContent = labels[status][en() ? 1 : 0];
    $("#undo-text").disabled = history.length === 0;
  }
  function paintMode() {
    pad.dataset.mode = mode;
    pad.setAttribute("aria-labelledby", `pointer-${mode}`);
    $$("[data-pointer]").forEach((button) => {
      const selected = button.dataset.pointer === mode;
      button.setAttribute("aria-selected", String(selected));
      button.tabIndex = selected ? 0 : -1;
    });
    $("#pointer-mode-number").textContent = `0${modes.indexOf(mode) + 1} / 03`;
    const hints = {
      touchpad: [
        "拖动移动 · 方向键也可体验",
        "Drag to move · Or use arrow keys",
      ],
      trackball: ["拖动红球移动指针", "Drag the ball to move the pointer"],
      trackpoint: ["按住向一个方向推", "Hold and push in a direction"],
    };
    $("#pointer-hint").textContent = hints[mode][en() ? 1 : 0];
  }
  function drawCursor() {
    cursor.style.left = `${pointer.x * 100}%`;
    cursor.style.top = `${pointer.y * 100}%`;
  }
  function move(x, y) {
    pointer.x = Math.max(0.03, Math.min(0.94, pointer.x + x));
    pointer.y = Math.max(0.05, Math.min(0.88, pointer.y + y));
    drawCursor();
    if (status !== "pointer") {
      status = "pointer";
      paintStatus();
    }
  }
  function stopGesture() {
    gesture = null;
    velocity = { x: 0, y: 0 };
    cancelAnimationFrame(animation);
    animation = 0;
  }
  function drift() {
    if (!gesture || mode !== "trackpoint") return;
    move(velocity.x, velocity.y);
    animation = requestAnimationFrame(drift);
  }
  input.value = t("继续，按刚才的方案做。", "Carry on with that plan.");
  input.addEventListener("input", paintInput);
  $("#send-text").addEventListener("click", () => {
    if (!input.value.length) {
      shortcut = "Enter";
      $("#key-feedback").textContent = shortcut;
      status = "key";
      paintStatus();
      return;
    }
    if (!input.value.trim()) return;
    history.push({ received, input: input.value });
    received = input.value.trim();
    $("#received-text").textContent = received;
    $("#received-text").removeAttribute("data-zh");
    $("#received-text").removeAttribute("data-en");
    $("#key-feedback").textContent = autoEnter ? "Enter" : "↗";
    status = "sent";
    paintStatus();
  });
  $$("[data-shortcut]").forEach((button) =>
    button.addEventListener("click", () => {
      shortcut = button.dataset.shortcut;
      $("#key-feedback").textContent = shortcut;
      status = "key";
      paintStatus();
    }),
  );
  $("#undo-text").addEventListener("click", () => {
    if (!history.length) return;
    const last = history.pop();
    received = last.received;
    input.value = last.input;
    $("#received-text").textContent =
      received ||
      t("你发来的文字会显示在这里。", "The text you send will appear here.");
    if (!received) {
      $("#received-text").dataset.zh = "你发来的文字会显示在这里。";
      $("#received-text").dataset.en = "The text you send will appear here.";
    }
    status = "undo";
    paintInput();
    paintStatus();
  });
  $("#voice-auto-toggle").addEventListener("click", (event) => {
    voiceAuto = !voiceAuto;
    event.currentTarget.setAttribute("aria-pressed", String(voiceAuto));
    status = "voice";
    paintStatus();
  });
  $("#auto-enter-toggle").addEventListener("click", (event) => {
    autoEnter = !autoEnter;
    event.currentTarget.setAttribute("aria-pressed", String(autoEnter));
    status = "auto";
    paintStatus();
  });
  $("#input-height-toggle").addEventListener("click", () => {
    inputHeight = (inputHeight + 1) % 3;
    $(".phone-composer").style.setProperty(
      "--demo-input-height",
      `${[112, 196, 280][inputHeight]}px`,
    );
  });
  $$("[data-pointer]").forEach((button) =>
    button.addEventListener("click", () => {
      stopGesture();
      mode = button.dataset.pointer;
      paintMode();
    }),
  );
  $(".pointer-tabs").addEventListener("keydown", (event) => {
    const current = modes.indexOf(mode);
    let next;
    if (event.key === "ArrowRight") next = (current + 1) % 3;
    else if (event.key === "ArrowLeft") next = (current + 2) % 3;
    else if (event.key === "Home") next = 0;
    else if (event.key === "End") next = 2;
    if (next !== undefined) {
      event.preventDefault();
      const target = $(`[data-pointer="${modes[next]}"]`);
      target.focus();
      target.click();
    }
  });
  pad.addEventListener("pointerdown", (event) => {
    if (event.button !== 0) return;
    const rect = pad.getBoundingClientRect();
    gesture = {
      id: event.pointerId,
      lastX: event.clientX,
      lastY: event.clientY,
      centerX: rect.left + rect.width / 2,
      centerY: rect.top + rect.height / 2,
      width: rect.width,
      height: rect.height,
    };
    pad.setPointerCapture(event.pointerId);
    if (mode === "trackpoint") {
      velocity = {
        x: Math.max(
          -0.012,
          Math.min(
            0.012,
            ((event.clientX - gesture.centerX) / gesture.width) * 0.035,
          ),
        ),
        y: Math.max(
          -0.012,
          Math.min(
            0.012,
            ((event.clientY - gesture.centerY) / gesture.height) * 0.035,
          ),
        ),
      };
      animation = requestAnimationFrame(drift);
    }
  });
  pad.addEventListener("pointermove", (event) => {
    if (!gesture || event.pointerId !== gesture.id) return;
    if (mode === "trackpoint") {
      velocity = {
        x: Math.max(
          -0.012,
          Math.min(
            0.012,
            ((event.clientX - gesture.centerX) / gesture.width) * 0.035,
          ),
        ),
        y: Math.max(
          -0.012,
          Math.min(
            0.012,
            ((event.clientY - gesture.centerY) / gesture.height) * 0.035,
          ),
        ),
      };
    } else {
      const sensitivity = mode === "trackball" ? 1.6 : 1;
      move(
        ((event.clientX - gesture.lastX) / gesture.width) * sensitivity,
        ((event.clientY - gesture.lastY) / gesture.height) * sensitivity,
      );
    }
    gesture.lastX = event.clientX;
    gesture.lastY = event.clientY;
  });
  pad.addEventListener("pointerup", stopGesture);
  pad.addEventListener("pointercancel", stopGesture);
  pad.addEventListener("lostpointercapture", stopGesture);
  pad.addEventListener("keydown", (event) => {
    const deltas = {
      ArrowLeft: [-0.04, 0],
      ArrowRight: [0.04, 0],
      ArrowUp: [0, -0.04],
      ArrowDown: [0, 0.04],
    };
    if (deltas[event.key]) {
      event.preventDefault();
      move(...deltas[event.key]);
    }
  });
  window.addEventListener("blur", stopGesture);
  document.addEventListener("visibilitychange", () => {
    if (document.hidden) stopGesture();
  });
  document.addEventListener("site:language", () => {
    paintInput();
    paintMode();
    paintStatus();
  });
  paintInput();
  paintMode();
  paintStatus();
  drawCursor();
})();
