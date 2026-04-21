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

export function initMobileListeners() {
  document.addEventListener('click', (e) => {
    const toggle = document.getElementById('mobile-menu-toggle');
    const menu = document.getElementById('mobile-menu');
    if (menu && toggle && !toggle.contains(e.target) && !menu.contains(e.target)) {
      closeMobileMenu();
    }
  });
}
