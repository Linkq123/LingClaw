export const ICON_NAMES = [
  'activity',
  'alert-triangle',
  'arrow-down',
  'arrow-up',
  'bolt',
  'chart',
  'check',
  'check-circle',
  'chevron-down',
  'chevron-left',
  'chevron-right',
  'circle-dot',
  'clipboard',
  'close',
  'copy',
  'database',
  'debug',
  'edit',
  'help',
  'info',
  'keyboard',
  'language',
  'layout',
  'menu',
  'message',
  'more',
  'package',
  'paperclip',
  'plus',
  'refresh',
  'reasoning',
  'search',
  'send',
  'settings',
  'skip',
  'stop',
  'task-plan',
  'todos',
  'tools',
  'trash',
  'user-node',
  'users',
  'workflow',
] as const;

export type IconName = (typeof ICON_NAMES)[number];

const SVG_NAMESPACE = 'http://www.w3.org/2000/svg';

export function iconHref(name: IconName): string {
  return `#icon-${name}`;
}

export function iconMarkup(name: IconName, className = 'icon'): string {
  return `<svg class="${className}" aria-hidden="true" focusable="false"><use href="${iconHref(name)}"></use></svg>`;
}

export function createIcon(name: IconName, className = 'icon'): SVGSVGElement {
  const svg = document.createElementNS(SVG_NAMESPACE, 'svg');
  svg.setAttribute('class', className);
  svg.setAttribute('aria-hidden', 'true');
  svg.setAttribute('focusable', 'false');
  const use = document.createElementNS(SVG_NAMESPACE, 'use');
  use.setAttribute('href', iconHref(name));
  svg.appendChild(use);
  return svg;
}
