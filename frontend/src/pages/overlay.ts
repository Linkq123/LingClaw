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
