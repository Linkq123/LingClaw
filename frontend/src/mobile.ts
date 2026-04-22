export function syncMobileMenuAria(open) {
  const toggle = document.getElementById('mobile-menu-toggle');
  if (toggle) toggle.setAttribute('aria-expanded', String(open));
}

export function toggleMobileMenu() {
  const menu = document.getElementById('mobile-menu');
  if (!menu) return;
  const willOpen = !menu.classList.contains('open');
  menu.classList.toggle('open', willOpen);
  syncMobileMenuAria(willOpen);
}

export function closeMobileMenu() {
  const menu = document.getElementById('mobile-menu');
  if (menu) menu.classList.remove('open');
  syncMobileMenuAria(false);
}

// Guard: prevent double-registration on Vite HMR re-execution of main.ts.
let _listenerInit = false;

export function initMobileListeners() {
  if (_listenerInit) return;
  _listenerInit = true;
  document.addEventListener('click', (e) => {
    const toggle = document.getElementById('mobile-menu-toggle');
    const menu = document.getElementById('mobile-menu');
    const target = e.target;
    if (!(target instanceof Node)) return;
    if (menu && toggle && !toggle.contains(target) && !menu.contains(target)) {
      closeMobileMenu();
    }
  });
}
