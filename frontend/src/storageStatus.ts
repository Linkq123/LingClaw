export const STORAGE_STATUS_EVENT = 'lingclaw:storage-status';

export type StorageMode = 'healthy' | 'protected';

export interface StorageStatusEventDetail {
  mode: StorageMode;
}

export function currentStorageMode(documentRef: Document = document): StorageMode {
  return documentRef.documentElement.dataset.storageMode === 'protected' ? 'protected' : 'healthy';
}

export function publishStorageStatus(
  mode: StorageMode,
  windowRef: Window = window,
  documentRef: Document = document,
): void {
  documentRef.documentElement.dataset.storageMode = mode;
  windowRef.dispatchEvent(
    new CustomEvent<StorageStatusEventDetail>(STORAGE_STATUS_EVENT, {
      detail: { mode },
    }),
  );
}

export function subscribeStorageStatus(
  listener: (mode: StorageMode) => void,
  windowRef: Window = window,
): () => void {
  const handleStatus = (event: Event) => {
    const mode = (event as CustomEvent<StorageStatusEventDetail>).detail?.mode;
    if (mode === 'healthy' || mode === 'protected') listener(mode);
  };
  windowRef.addEventListener(STORAGE_STATUS_EVENT, handleStatus);
  return () => windowRef.removeEventListener(STORAGE_STATUS_EVENT, handleStatus);
}
