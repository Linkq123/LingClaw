import { afterNextPaint } from '../utils.js';

let collapsibleId = 0;

export function linkCollapsibleControl(
  trigger: HTMLElement,
  body: HTMLElement,
  prefix = 'collapsible',
): void {
  if (!body.id) body.id = `${prefix}-${++collapsibleId}`;
  trigger.setAttribute('aria-controls', body.id);
  trigger.setAttribute('aria-expanded', String(body.classList.contains('show')));
}

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
  if (wrapper) wrapper.remove();
  else panel.remove();
}

export function animateCollapsibleSection(body, expand) {
  if (!body) return;

  if (body.id && body.parentElement) {
    const controls = body.parentElement.querySelectorAll(
      '[aria-controls]',
    ) as NodeListOf<HTMLElement>;
    const control = Array.from(controls).find(
      (candidate) => candidate.getAttribute('aria-controls') === body.id,
    );
    control?.setAttribute('aria-expanded', String(expand));
  }

  const startHeight = body.getBoundingClientRect().height;
  body.classList.toggle('show', expand);

  // When expanding, clamp target to CSS max-height (if any) so the
  // animation ends exactly at the visible cap instead of overshooting
  // invisibly — otherwise the user sees a pause while height transitions
  // past the visible region.
  let targetHeight = expand ? body.scrollHeight : 0;
  if (expand) {
    const maxH = parseFloat(getComputedStyle(body).maxHeight);
    if (Number.isFinite(maxH) && maxH > 0 && targetHeight > maxH) {
      targetHeight = maxH;
    }
  }

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
