const FOCUSABLE_SELECTOR = [
  'a[href]',
  'button:not([disabled])',
  'input:not([disabled])',
  'select:not([disabled])',
  'textarea:not([disabled])',
  '[tabindex]:not([tabindex="-1"])',
].join(',');

function isVisibleWithin(element: HTMLElement, container: HTMLElement): boolean {
  let current: HTMLElement | null = element;
  while (current && container.contains(current)) {
    if (current.hidden || current.inert || current.getAttribute('aria-hidden') === 'true') {
      return false;
    }
    const style = window.getComputedStyle(current);
    if (style.display === 'none' || style.visibility === 'hidden') return false;
    current = current.parentElement;
  }
  return true;
}

export function trapDialogFocus(event: KeyboardEvent, container: HTMLElement | null): boolean {
  if (event.key !== 'Tab' || !container) return false;
  const focusable = Array.from(container.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR)).filter(
    (element) => isVisibleWithin(element, container),
  );
  if (focusable.length === 0) {
    event.preventDefault();
    container.focus();
    return true;
  }

  const first = focusable[0];
  const last = focusable[focusable.length - 1];
  const active = document.activeElement;
  if (!container.contains(active)) {
    event.preventDefault();
    (event.shiftKey ? last : first).focus();
    return true;
  }
  if (event.shiftKey && active === first) {
    event.preventDefault();
    last.focus();
    return true;
  }
  if (!event.shiftKey && active === last) {
    event.preventDefault();
    first.focus();
    return true;
  }
  return false;
}
