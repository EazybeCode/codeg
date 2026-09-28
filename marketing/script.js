/**
 * Neev Marketing Site — Minimal JavaScript
 * Mobile navigation and accessible tab panel for workspace preview
 */

(function () {
  "use strict";

  // Mobile navigation
  const navToggle = document.querySelector(".mobile-nav-toggle");
  const navMenu = document.getElementById("nav-menu");

  if (navToggle && navMenu) {
    navToggle.addEventListener("click", function () {
      const isOpen = navToggle.getAttribute("aria-expanded") === "true";
      navToggle.setAttribute("aria-expanded", String(!isOpen));
      navToggle.setAttribute(
        "aria-label",
        isOpen ? "Open menu" : "Close menu"
      );
      navMenu.classList.toggle("open", !isOpen);
    });

    // Close menu when clicking a link
    navMenu.querySelectorAll("a").forEach(function (link) {
      link.addEventListener("click", function () {
        navToggle.setAttribute("aria-expanded", "false");
        navToggle.setAttribute("aria-label", "Open menu");
        navMenu.classList.remove("open");
      });
    });

    // Close menu on escape key
    document.addEventListener("keydown", function (e) {
      if (e.key === "Escape" && navMenu.classList.contains("open")) {
        navToggle.setAttribute("aria-expanded", "false");
        navToggle.setAttribute("aria-label", "Open menu");
        navMenu.classList.remove("open");
        navToggle.focus();
      }
    });
  }

  // Accessible tab panel for workspace preview
  const tablist = document.querySelector('[role="tablist"]');
  const tabs = tablist
    ? Array.from(tablist.querySelectorAll('[role="tab"]'))
    : [];
  const panels = tabs.map(function (tab) {
    return document.getElementById(tab.getAttribute("aria-controls"));
  });

  function activateTab(tab) {
    // Deactivate all tabs
    tabs.forEach(function (t) {
      t.setAttribute("aria-selected", "false");
      t.classList.remove("active");
      t.setAttribute("tabindex", "-1");
    });

    // Hide all panels
    panels.forEach(function (panel) {
      if (panel) {
        panel.hidden = true;
        panel.classList.remove("active");
      }
    });

    // Activate selected tab
    tab.setAttribute("aria-selected", "true");
    tab.classList.add("active");
    tab.setAttribute("tabindex", "0");

    // Show corresponding panel
    var panelId = tab.getAttribute("aria-controls");
    var panel = document.getElementById(panelId);
    if (panel) {
      panel.hidden = false;
      panel.classList.add("active");
    }
  }

  tabs.forEach(function (tab, index) {
    // Set initial tabindex
    tab.setAttribute("tabindex", index === 0 ? "0" : "-1");

    // Click handler
    tab.addEventListener("click", function () {
      activateTab(tab);
    });

    // Keyboard navigation
    tab.addEventListener("keydown", function (e) {
      var currentIndex = tabs.indexOf(tab);
      var newIndex;

      switch (e.key) {
        case "ArrowLeft":
          e.preventDefault();
          newIndex = currentIndex === 0 ? tabs.length - 1 : currentIndex - 1;
          tabs[newIndex].focus();
          activateTab(tabs[newIndex]);
          break;

        case "ArrowRight":
          e.preventDefault();
          newIndex = currentIndex === tabs.length - 1 ? 0 : currentIndex + 1;
          tabs[newIndex].focus();
          activateTab(tabs[newIndex]);
          break;

        case "Home":
          e.preventDefault();
          tabs[0].focus();
          activateTab(tabs[0]);
          break;

        case "End":
          e.preventDefault();
          tabs[tabs.length - 1].focus();
          activateTab(tabs[tabs.length - 1]);
          break;
      }
    });
  });
})();

// A lightweight, explicitly illustrative product demo. Never runs a real agent.
(function () {
  const preview = document.querySelector('.preview-window');
  const toggle = document.getElementById('demo-toggle');
  const status = document.getElementById('demo-status');
  const response = document.getElementById('demo-response');
  const timer = document.querySelector('.demo-timer');
  const reduced = window.matchMedia('(prefers-reduced-motion: reduce)');
  if (!preview || !toggle || !status || !response) return;
  let enabled = !reduced.matches;
  let visible = false;
  let elapsed = 0;
  let interval;
  const steps = [
    ['Reading the attached brief', 'I’ll read the brief and turn it into a campaign plan your team can review.'],
    ['Using your team’s brand voice', 'I’ve reviewed the product notes. I’m shaping the launch story around your audience and writing guidelines.'],
    ['Saving the first draft', 'The first draft is ready. It covers positioning, launch channels, and next steps for the team.'],
    ['Draft ready to review', 'Saved launch-brief.html to this conversation. Your team can review the draft here and ask for the next version.']
  ];
  function draw() {
    const stage = Math.min(3, Math.floor(elapsed / 4));
    status.textContent = steps[stage][0];
    response.textContent = steps[stage][1];
    timer.textContent = '0:' + String(Math.min(elapsed,12)).padStart(2,'0');
    preview.classList.toggle('demo-complete', stage === 3);
  }
  function refresh() {
    clearInterval(interval);
    preview.classList.toggle('demo-playing', enabled && visible);
    toggle.textContent = enabled ? 'Pause preview' : 'Play preview';
    toggle.setAttribute('aria-pressed', String(!enabled));
    if (enabled && visible && !document.hidden) {
      interval = setInterval(function () { elapsed = (elapsed + 1) % 20; draw(); }, 1000);
    }
  }
  toggle.addEventListener('click', function () {
    if (!enabled) document.getElementById('tab-chat').click();
    enabled = !enabled; refresh();
  });
  document.querySelectorAll('[role="tab"]').forEach(function(tab) {
    function stop() { enabled = false; refresh(); }
    tab.addEventListener('click', stop);
    tab.addEventListener('keydown', function(e) {
      if (['ArrowLeft','ArrowRight','Home','End'].includes(e.key)) stop();
    });
  });
  if ('IntersectionObserver' in window) {
    new IntersectionObserver(function(entries) { visible = entries[0].isIntersecting; refresh(); }, {threshold:0.2}).observe(preview);
  } else { visible=true; }
  document.addEventListener('visibilitychange',refresh);
  reduced.addEventListener('change',function() { if (reduced.matches) { enabled=false; elapsed=12; draw(); refresh(); } });
  if (reduced.matches) elapsed=12;
  draw(); refresh();
})();
