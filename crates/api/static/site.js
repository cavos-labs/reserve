// Shared page chrome: the scroll-edge rule on the sticky bar, and scroll reveals.
// Both are enhancements. Content is visible without this file running at all.
(() => {
  const bar = document.querySelector(".bar");
  if (bar) {
    const lift = () => {
      bar.dataset.lifted = String(window.scrollY > 8);
    };
    lift();
    addEventListener("scroll", lift, { passive: true });
  }

  const items = document.querySelectorAll(".await");
  if (!items.length) return;

  const show = () => {
    for (const el of items) el.classList.add("seen");
  };

  if (matchMedia("(prefers-reduced-motion: reduce)").matches) return;

  document.documentElement.classList.add("js-motion");

  if (!("IntersectionObserver" in window)) {
    show();
    return;
  }

  const io = new IntersectionObserver(
    (entries) => {
      for (const e of entries) {
        if (!e.isIntersecting) continue;
        e.target.classList.add("seen");
        io.unobserve(e.target);
      }
    },
    { rootMargin: "0px 0px -8% 0px", threshold: 0.05 },
  );

  for (const el of items) {
    // Stagger only within a group of siblings; each reveal stays under 200ms of lag.
    const siblings = [...(el.parentElement?.children ?? [])].filter((n) =>
      n.classList.contains("await"),
    );
    el.style.setProperty("--d", `${Math.min(siblings.indexOf(el), 3) * 0.06}s`);
    io.observe(el);
  }

  // Watchdog: a reveal that never fires must never cost anyone the content.
  setTimeout(show, 2500);
  addEventListener("pagehide", show);
})();
