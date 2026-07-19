export const WORKSPACE_PORTAL_ROOT_ID = 'workspace-portal-root';
export const CONSOLE_PAGE_ID = 'console-page';

export function isConsoleSurfaceActive(documentRef: Document = document): boolean {
  const consolePage = documentRef.getElementById(CONSOLE_PAGE_ID);
  return consolePage !== null && !consolePage.hidden;
}

export function getWorkspacePortalRoot(documentRef: Document = document): HTMLElement {
  return documentRef.getElementById(WORKSPACE_PORTAL_ROOT_ID) || documentRef.body;
}

export function appendWorkspacePortal(
  element: HTMLElement,
  documentRef: Document = element.ownerDocument,
): void {
  getWorkspacePortalRoot(documentRef).appendChild(element);
}
