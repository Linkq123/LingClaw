export type ComposerPopoverKind = 'attachments' | 'models';

type ComposerPopoverCloser = (returnFocus?: boolean) => void;

const popoverClosers = new Map<ComposerPopoverKind, ComposerPopoverCloser>();

export function registerComposerPopover(
  kind: ComposerPopoverKind,
  close: ComposerPopoverCloser,
): void {
  popoverClosers.set(kind, close);
}

export function closeComposerPopovers(except?: ComposerPopoverKind, returnFocus = false): void {
  popoverClosers.forEach((close, kind) => {
    if (kind !== except) close(returnFocus);
  });
}
