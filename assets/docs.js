"use strict";
(() => {
  const headings = [...document.querySelectorAll('.doc-content h2[id]')];
  const links = [...document.querySelectorAll('.page-toc nav a, .mobile-page-toc nav a')];
  if (!headings.length) return;
  const markCurrent = () => {
    let current = headings[0];
    for (const heading of headings) {
      if (heading.getBoundingClientRect().top <= 180) current = heading;
      else break;
    }
    for (const link of links) {
      if (link.hash === `#${current.id}`) link.setAttribute('aria-current', 'location');
      else link.removeAttribute('aria-current');
    }
  };
  const observer = new IntersectionObserver(markCurrent, { rootMargin: '-120px 0px -55% 0px', threshold: 0 });
  headings.forEach(heading => observer.observe(heading));
  addEventListener('hashchange', markCurrent);
  markCurrent();
})();
