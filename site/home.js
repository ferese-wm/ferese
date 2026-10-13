const demo = document.querySelector('#desktop-demo');
const reducedMotion = window.matchMedia('(prefers-reduced-motion: reduce)');
let started = false;

// Start muted when the demo comes into view. Native controls remain available
// without JavaScript and after a visitor pauses playback.
const visibility = new IntersectionObserver(([entry]) => {
  if (!entry.isIntersecting) {
    demo.pause();
  } else if (!started && !reducedMotion.matches) {
    started = true;
    demo.play().catch(() => {});
  }
}, { threshold: 0.25 });
visibility.observe(demo);
reducedMotion.addEventListener('change', () => {
  if (reducedMotion.matches) demo.pause();
});
