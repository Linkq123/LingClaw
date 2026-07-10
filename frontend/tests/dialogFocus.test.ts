import { beforeEach, describe, expect, it } from 'vitest';
import { trapDialogFocus } from '../src/pages/dialogFocus.js';

describe('dialog focus trap', () => {
  beforeEach(() => {
    document.body.innerHTML = `
      <div id="dialog" tabindex="-1">
        <button id="first">First</button>
        <button id="last">Last</button>
      </div>
    `;
  });

  it('wraps forward focus from the last control to the first', () => {
    const dialog = document.getElementById('dialog') as HTMLElement;
    const first = document.getElementById('first') as HTMLButtonElement;
    const last = document.getElementById('last') as HTMLButtonElement;
    last.focus();
    const event = new KeyboardEvent('keydown', { key: 'Tab', cancelable: true });

    expect(trapDialogFocus(event, dialog)).toBe(true);
    expect(document.activeElement).toBe(first);
    expect(event.defaultPrevented).toBe(true);
  });

  it('wraps backward focus from the first control to the last', () => {
    const dialog = document.getElementById('dialog') as HTMLElement;
    const first = document.getElementById('first') as HTMLButtonElement;
    const last = document.getElementById('last') as HTMLButtonElement;
    first.focus();
    const event = new KeyboardEvent('keydown', {
      key: 'Tab',
      shiftKey: true,
      cancelable: true,
    });

    expect(trapDialogFocus(event, dialog)).toBe(true);
    expect(document.activeElement).toBe(last);
  });

  it('ignores focusable controls inside hidden tab panels', () => {
    document.body.innerHTML = `
      <div id="dialog" tabindex="-1">
        <button id="first">First</button>
        <button id="last">Last visible</button>
        <section hidden><button id="hidden-last">Hidden tab control</button></section>
      </div>
    `;
    const dialog = document.getElementById('dialog') as HTMLElement;
    const first = document.getElementById('first') as HTMLButtonElement;
    const last = document.getElementById('last') as HTMLButtonElement;
    last.focus();
    const event = new KeyboardEvent('keydown', { key: 'Tab', cancelable: true });

    expect(trapDialogFocus(event, dialog)).toBe(true);
    expect(document.activeElement).toBe(first);
  });

  it('recovers focus into the dialog when focus starts outside', () => {
    const dialog = document.getElementById('dialog') as HTMLElement;
    const first = document.getElementById('first') as HTMLButtonElement;
    document.body.focus();
    const event = new KeyboardEvent('keydown', { key: 'Tab', cancelable: true });

    expect(trapDialogFocus(event, dialog)).toBe(true);
    expect(document.activeElement).toBe(first);
  });
});
