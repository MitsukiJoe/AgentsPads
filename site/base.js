"use strict";

(() => {
  const root = document.documentElement;
  const key = document.body.dataset.product;
  const $ = (selector) => document.querySelector(selector);
  const remember = (name, value) => {
    try {
      localStorage.setItem(`${key}-${name}`, value);
    } catch {}
  };
  const en = () => root.lang === "en";
  const menu = $("#site-menu");
  let copyTimer;
  function closeMenu() {
    $("#site-nav").classList.remove("open");
    menu.setAttribute("aria-expanded", "false");
  }
  function theme() {
    const dark = root.dataset.theme === "dark";
    $("#theme-toggle").innerHTML = dark
      ? '<svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="4"/><path d="M12 2v2m0 16v2M2 12h2m16 0h2M5 5l1.4 1.4m11.2 11.2L19 19M5 19l1.4-1.4M17.6 6.4 19 5"/></svg>'
      : '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M20 14A9 9 0 0 1 10 4a9 9 0 1 0 10 10Z"/></svg>';
    $("#theme-toggle").setAttribute(
      "aria-label",
      en()
        ? `Switch to ${dark ? "light" : "dark"} mode`
        : `切换到${dark ? "浅" : "深"}色模式`,
    );
  }
  function language() {
    document.querySelectorAll("[data-zh][data-en]").forEach((element) => {
      element.innerHTML = element.dataset[en() ? "en" : "zh"];
    });
    document
      .querySelectorAll("[data-zh-label]")
      .forEach((element) =>
        element.setAttribute(
          "aria-label",
          element.dataset[en() ? "enLabel" : "zhLabel"],
        ),
      );
    document.title = root.dataset[en() ? "titleEn" : "titleZh"];
    $('meta[name="description"]').content =
      root.dataset[en() ? "descriptionEn" : "descriptionZh"];
    $('meta[property="og:title"]').content = document.title;
    $('meta[property="og:description"]').content = $(
      'meta[name="description"]',
    ).content;
    $("#language-toggle").textContent = en() ? "中文" : "EN";
    $("#language-toggle").setAttribute(
      "aria-label",
      en() ? "切换到中文" : "Switch to English",
    );
    theme();
    document.dispatchEvent(new Event("site:language"));
  }
  $("#language-toggle").addEventListener("click", () => {
    root.lang = en() ? "zh-CN" : "en";
    remember("language", root.lang);
    language();
    closeMenu();
  });
  $("#theme-toggle").addEventListener("click", () => {
    root.dataset.theme = root.dataset.theme === "dark" ? "light" : "dark";
    remember("theme", root.dataset.theme);
    theme();
  });
  matchMedia("(prefers-color-scheme: dark)").addEventListener(
    "change",
    (event) => {
      try {
        if (localStorage.getItem(`${key}-theme`)) return;
      } catch {}
      root.dataset.theme = event.matches ? "dark" : "light";
      theme();
    },
  );
  menu.addEventListener("click", () => {
    const open = menu.getAttribute("aria-expanded") !== "true";
    $("#site-nav").classList.toggle("open", open);
    menu.setAttribute("aria-expanded", String(open));
  });
  document
    .querySelectorAll("#site-nav a")
    .forEach((link) => link.addEventListener("click", closeMenu));
  document.addEventListener("keydown", (event) => {
    if (
      event.key === "Escape" &&
      menu.getAttribute("aria-expanded") === "true"
    ) {
      closeMenu();
      menu.focus();
    }
  });
  document.addEventListener("click", (event) => {
    if (!event.target.closest(".site-header")) closeMenu();
  });
  matchMedia("(max-width: 700px)").addEventListener("change", closeMenu);
  document.querySelectorAll("[data-copy]").forEach((button) =>
    button.addEventListener("click", async () => {
      clearTimeout(copyTimer);
      try {
        await navigator.clipboard.writeText(
          document.querySelector(button.dataset.copy).textContent.trim(),
        );
        button.textContent = en() ? "Copied ✓" : "已复制 ✓";
        $("#announcement").textContent = button.textContent;
        copyTimer = setTimeout(() => {
          button.textContent = en() ? "Copy" : "复制";
        }, 2000);
      } catch {
        const range = document.createRange();
        range.selectNodeContents(document.querySelector(button.dataset.copy));
        const selection = window.getSelection();
        selection.removeAllRanges();
        selection.addRange(range);
        button.textContent = en() ? "Select to copy" : "请手动复制";
        $("#announcement").textContent = en()
          ? "Clipboard unavailable. The command is selected for manual copying."
          : "无法访问剪贴板，已选中命令，请手动复制。";
      }
    }),
  );
  language();
  const reduced = matchMedia("(prefers-reduced-motion: reduce)");
  if ("IntersectionObserver" in window && !reduced.matches) {
    const observer = new IntersectionObserver(
      (entries) =>
        entries.forEach((entry) => {
          if (entry.isIntersecting) {
            entry.target.classList.remove("pending");
            observer.unobserve(entry.target);
          }
        }),
      { threshold: 0.06 },
    );
    document.querySelectorAll(".reveal").forEach((element) => {
      element.classList.add("pending");
      observer.observe(element);
    });
    reduced.addEventListener("change", (event) => {
      if (event.matches) {
        document
          .querySelectorAll(".pending")
          .forEach((element) => element.classList.remove("pending"));
        observer.disconnect();
      }
    });
  }
})();
