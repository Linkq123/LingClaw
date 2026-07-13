import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { dismissActionDialog, openActionDialog, refreshActionDialog } from '../src/actionDialog.js';
import { setLanguage } from '../src/i18n.js';

describe('application action dialog', () => {
  beforeEach(() => {
    dismissActionDialog();
    document.body.innerHTML = '<button id="opener">Open</button>';
    document.getElementById('opener')?.focus();
    setLanguage('en');
  });

  afterEach(() => {
    dismissActionDialog();
    setLanguage('en');
    document.body.innerHTML = '';
  });

  it('submits a renamed Session and restores focus', async () => {
    const submit = vi.fn().mockResolvedValue(undefined);
    const resultPromise = openActionDialog({
      kind: 'rename-session',
      entityId: 'worker-a',
      entityName: 'Worker A',
      submit,
    });
    await Promise.resolve();

    const input = document.querySelector<HTMLInputElement>('.action-dialog-field input')!;
    expect(document.activeElement).toBe(input);
    input.value = '   ';
    input.dispatchEvent(new Event('input', { bubbles: true }));
    input.closest('form')?.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }));
    const validatedInput = document.querySelector<HTMLInputElement>('.action-dialog-field input')!;
    expect(validatedInput.getAttribute('aria-invalid')).toBe('true');
    expect(validatedInput.getAttribute('aria-describedby')).toBe('action-dialog-error');
    validatedInput.value = 'Frontend';
    validatedInput.dispatchEvent(new Event('input', { bubbles: true }));
    expect(validatedInput.hasAttribute('aria-invalid')).toBe(false);
    expect(document.getElementById('action-dialog-error')?.hidden).toBe(true);
    validatedInput
      .closest('form')
      ?.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }));

    await expect(resultPromise).resolves.toEqual({ kind: 'rename-session', name: 'Frontend' });
    expect(submit).toHaveBeenCalledWith('Frontend');
    expect(document.querySelector('.action-dialog-overlay')).toBeNull();
    await Promise.resolve();
    expect(document.activeElement).toBe(document.getElementById('opener'));
  });

  it('validates group members, excludes Main, and returns typed member ids', async () => {
    const submit = vi.fn().mockResolvedValue(undefined);
    const resultPromise = openActionDialog({
      kind: 'create-group',
      initialName: 'Review group',
      sessions: [
        { id: 'worker-a', name: 'Frontend' },
        { id: 'worker-b', name: 'Backend' },
      ],
      selectedMembers: ['main', 'deleted-session'],
      submit,
    });
    await Promise.resolve();

    expect(document.querySelector('.action-dialog-owner')?.textContent).toContain(
      'Main · permanent owner',
    );
    expect(document.querySelector('.action-dialog-members-header span')?.textContent).toBe(
      '0 selected',
    );
    const form = document.querySelector<HTMLFormElement>('.action-dialog-form')!;
    form.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }));
    expect(document.querySelector('.action-dialog-error')?.textContent).toBe(
      'Select at least one dispatch member.',
    );
    const members = document.querySelector<HTMLElement>('.action-dialog-members')!;
    expect(members.getAttribute('aria-invalid')).toBe('true');
    expect(members.getAttribute('aria-describedby')).toBe('action-dialog-error');
    await Promise.resolve();
    expect(document.activeElement).toBe(
      document.querySelector<HTMLInputElement>('.action-dialog-search input'),
    );

    const worker = document.querySelector<HTMLInputElement>(
      '.action-dialog-member-option input[value="worker-b"]',
    )!;
    worker.checked = true;
    worker.dispatchEvent(new Event('change', { bubbles: true }));
    expect(members.hasAttribute('aria-invalid')).toBe(false);
    expect(document.getElementById('action-dialog-error')?.hidden).toBe(true);
    expect(document.querySelector('.action-dialog-members-header span')?.textContent).toBe(
      '1 selected',
    );
    document
      .querySelector<HTMLFormElement>('.action-dialog-form')
      ?.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }));

    await expect(resultPromise).resolves.toEqual({
      kind: 'create-group',
      name: 'Review group',
      members: ['worker-b'],
    });
    expect(submit).toHaveBeenCalledWith({ name: 'Review group', members: ['worker-b'] });
  });

  it('keeps async failures inline and refreshes an open dialog language', async () => {
    const submit = vi
      .fn<() => Promise<void>>()
      .mockRejectedValueOnce(new Error('offline'))
      .mockResolvedValueOnce(undefined);
    const resultPromise = openActionDialog({
      kind: 'delete-group',
      entityId: 'review-group',
      entityName: 'Review Group',
      submit,
    });
    await Promise.resolve();

    document
      .querySelector<HTMLFormElement>('.action-dialog-form')
      ?.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }));
    await vi.waitFor(() => {
      expect(document.querySelector('.action-dialog-error')?.textContent).toContain('offline');
    });
    expect(document.querySelector('.action-dialog-overlay')).not.toBeNull();
    expect(document.querySelector('.action-dialog-panel')?.getAttribute('aria-describedby')).toBe(
      'action-dialog-error',
    );

    setLanguage('zh-CN');
    refreshActionDialog();
    await Promise.resolve();
    expect(document.getElementById('action-dialog-title')?.textContent).toBe('删除群聊');
    expect(document.getElementById('action-dialog-error')?.textContent).toBe('操作失败：offline');

    document
      .querySelector<HTMLFormElement>('.action-dialog-form')
      ?.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }));
    await expect(resultPromise).resolves.toEqual({ kind: 'delete-group' });
    expect(submit).toHaveBeenCalledTimes(2);
  });

  it('traps focus and ignores backdrop or Escape while submitting', async () => {
    let resolveSubmit!: () => void;
    const submit = vi.fn(
      () =>
        new Promise<void>((resolve) => {
          resolveSubmit = resolve;
        }),
    );
    const resultPromise = openActionDialog({
      kind: 'delete-session',
      entityId: 'worker-a',
      entityName: 'Worker A',
      submit,
    });
    await Promise.resolve();
    const overlay = document.querySelector<HTMLElement>('.action-dialog-overlay')!;
    const form = document.querySelector<HTMLFormElement>('.action-dialog-form')!;
    const submitButton = form.querySelector<HTMLButtonElement>('.action-dialog-submit')!;
    const closeButton = document.querySelector<HTMLButtonElement>('.action-dialog-close')!;
    submitButton.focus();
    overlay.dispatchEvent(
      new KeyboardEvent('keydown', { key: 'Tab', bubbles: true, cancelable: true }),
    );
    expect(document.activeElement).toBe(closeButton);
    form.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }));
    await Promise.resolve();
    await Promise.resolve();
    expect(document.activeElement).toBe(document.querySelector('.action-dialog-panel'));

    overlay.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    overlay.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
    expect(document.querySelector('.action-dialog-overlay')).toBe(overlay);

    resolveSubmit();
    await expect(resultPromise).resolves.toEqual({ kind: 'delete-session' });
  });

  it('keeps one active dialog and cancels with Escape when idle', async () => {
    const first = openActionDialog({
      kind: 'rename-session',
      entityId: 'worker-a',
      entityName: 'Worker A',
      submit: vi.fn(),
    });
    await Promise.resolve();
    const duplicate = openActionDialog({
      kind: 'delete-session',
      entityId: 'worker-b',
      entityName: 'Worker B',
      submit: vi.fn(),
    });

    await expect(duplicate).resolves.toBeNull();
    expect(document.getElementById('action-dialog-title')?.textContent).toBe('Rename session');
    document
      .querySelector<HTMLElement>('.action-dialog-overlay')
      ?.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
    await expect(first).resolves.toBeNull();
  });

  it('submits confirmation dialogs with Enter from the dialog surface', async () => {
    const submit = vi.fn().mockResolvedValue(undefined);
    const resultPromise = openActionDialog({
      kind: 'delete-group',
      entityId: 'review-group',
      entityName: 'Review Group',
      submit,
    });
    await Promise.resolve();
    const panel = document.querySelector<HTMLElement>('.action-dialog-panel')!;

    panel.dispatchEvent(
      new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true }),
    );

    await expect(resultPromise).resolves.toEqual({ kind: 'delete-group' });
    expect(submit).toHaveBeenCalledOnce();
  });
});
