export function closeOverlayById(
  overlayId: string | null | undefined,
  closeSettingsPage: () => void,
  closeUsagePage: () => void,
): boolean {
  switch (overlayId) {
    case 'settings-page':
      closeSettingsPage();
      return true;
    case 'usage-page':
      closeUsagePage();
      return true;
    default:
      return false;
  }
}

export function matchesOverlayDismissTarget(
  target: EventTarget | null,
  overlay: Element,
  controlSelector: string,
): boolean {
  return (
    target instanceof Element &&
    (target === overlay || target.closest(controlSelector) !== null)
  );
}
