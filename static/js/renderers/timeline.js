import { afterNextPaint } from '../utils.js';

export function animatePanelIn(panel) {
  if (!panel) return;
  panel.classList.add('panel-enter');
  afterNextPaint(() => {
    if (!panel.isConnected) return;
    panel.classList.add('panel-enter-active');
  });
}

export function wrapInTimeline(panel, variant) {
  const node = document.createElement('div');
  node.className = 'timeline-node' + (variant ? ` timeline-node--${variant}` : '');
  node.appendChild(panel);
  return node;
}

export function removeTimelinePanel(panel) {
  if (!panel) return;
  const wrapper = panel.closest('.timeline-node');
  if (wrapper) wrapper.remove(); else panel.remove();
}

export function animateCollapsibleSection(body, expand) {
  if (!body) return;

  const startHeight = body.getBoundingClientRect().height;
  body.classList.toggle('show', expand);
  const targetHeight = expand ? body.scrollHeight : 0;

  body.style.height = `${startHeight}px`;
  body.getBoundingClientRect();
  body.classList.toggle('is-open', expand);
  body.style.height = `${targetHeight}px`;

  const finalize = (e) => {
    if (e.propertyName !== 'height') return;
    body.style.height = expand ? 'auto' : '0px';
    body.removeEventListener('transitionend', finalize);
  };

  body.addEventListener('transitionend', finalize);
}
