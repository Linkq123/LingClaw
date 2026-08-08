import React, { act } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createRoot, type Root } from 'react-dom/client';

import { setLanguage } from '../src/i18n.js';
import { ModelsConsole } from '../src/pages/ModelsConsole.js';
import { THINKING_EFFORT_LEVELS } from '../src/types/config.js';
import type { AppConfig } from '../src/types/config.js';

const config = {
  models: {
    providers: {
      gateway: {
        api: 'openai-completions',
        baseUrl: 'https://gateway.example/v1',
        apiKey: 'secret',
        region: 'local',
        models: [
          {
            id: 'vision-model',
            name: 'Vision model',
            input: ['text', 'image'],
            reasoning: true,
            contextWindow: 128000,
            maxTokens: 16000,
            compat: { thinkingFormat: 'custom-gateway', passthrough: true },
            priceTier: 'internal',
          },
          { id: 'text-model', input: ['text'] },
        ],
      },
    },
  },
} as AppConfig;

function setInputValue(input: HTMLInputElement | HTMLTextAreaElement, value: string): void {
  const prototype =
    input instanceof HTMLTextAreaElement
      ? window.HTMLTextAreaElement.prototype
      : window.HTMLInputElement.prototype;
  Object.getOwnPropertyDescriptor(prototype, 'value')?.set?.call(input, value);
  input.dispatchEvent(new Event('input', { bubbles: true }));
}

async function flush(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
}

describe('ModelsConsole', () => {
  let root: Root;
  let container: HTMLDivElement;

  beforeEach(async () => {
    (
      globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }
    ).IS_REACT_ACT_ENVIRONMENT = true;
    setLanguage('en');
    Object.defineProperty(window, 'matchMedia', {
      configurable: true,
      value: vi.fn().mockReturnValue({ matches: false }),
    });
    container = document.createElement('div');
    document.body.replaceChildren(container);
    root = createRoot(container);
  });

  afterEach(async () => {
    await act(async () => root.unmount());
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean })
      .IS_REACT_ACT_ENVIRONMENT;
    vi.restoreAllMocks();
  });

  async function render(
    onChange = vi.fn(),
    onStatus = vi.fn(),
    options: {
      config?: AppConfig;
      baselineRevision?: number;
      onDraftDirtyChange?: ReturnType<typeof vi.fn>;
    } = {},
  ) {
    const renderProps = async (nextConfig: AppConfig, baselineRevision = 0) => {
      await act(async () => {
        root.render(
          React.createElement(ModelsConsole, {
            config: nextConfig,
            onChange,
            onStatus,
            baselineRevision,
            onDraftDirtyChange: options.onDraftDirtyChange,
          }),
        );
        await flush();
      });
    };
    await renderProps(options.config || config, options.baselineRevision || 0);
    return { onChange, onStatus, rerender: renderProps };
  }

  it('edits a model card while preserving unknown metadata and a custom thinking format', async () => {
    const { onChange } = await render();
    const card = container.querySelector<HTMLButtonElement>('.models-console-card');
    expect(card?.textContent).toContain('Vision model');

    await act(async () => card?.click());
    const modelId = container.querySelector<HTMLInputElement>(
      '.models-console-inspector input[placeholder="model-id"]',
    );
    const thinking = container.querySelector<HTMLInputElement>(
      '.models-console-inspector input[list="models-console-thinking-formats"]',
    );
    expect(thinking?.value).toBe('custom-gateway');

    await act(async () => {
      setInputValue(modelId!, 'vision-model-v2');
      await flush();
    });

    const latest = onChange.mock.calls.at(-1)?.[0] as AppConfig;
    const provider = latest.models?.providers?.gateway as Record<string, unknown>;
    const models = provider.models as Array<Record<string, unknown>>;
    expect(provider.region).toBe('local');
    expect(models[0]).toMatchObject({
      id: 'vision-model-v2',
      priceTier: 'internal',
      compat: { thinkingFormat: 'custom-gateway', passthrough: true },
    });
  });

  it('configures supported reasoning efforts and their default from the model inspector', async () => {
    const { onChange } = await render();
    const card = container.querySelector<HTMLButtonElement>('.models-console-card')!;
    expect(card.textContent).toContain('Auto');
    expect(card.textContent).toContain('8 levels');

    await act(async () => card.click());
    const effortEditor = container.querySelector<HTMLElement>('.models-console-effort-editor')!;
    const effortInputs = Array.from(
      effortEditor.querySelectorAll<HTMLInputElement>('.models-console-effort-levels input'),
    );
    expect(effortInputs).toHaveLength(8);
    expect(effortInputs.every((input) => input.checked)).toBe(true);

    const auto = effortInputs.find((input) => input.parentElement?.textContent?.trim() === 'Auto')!;
    await act(async () => {
      auto.click();
      await flush();
    });
    let latest = onChange.mock.calls.at(-1)?.[0] as AppConfig;
    expect(latest.models?.providers?.gateway.models?.[0].effort).toEqual({
      levels: ['off', 'minimal', 'low', 'medium', 'high', 'xhigh', 'max'],
      default: 'off',
    });

    const defaultEffort = effortEditor.querySelector<HTMLSelectElement>(
      '.models-console-effort-default select',
    )!;
    await act(async () => {
      defaultEffort.value = 'high';
      defaultEffort.dispatchEvent(new Event('change', { bubbles: true }));
      await flush();
    });
    latest = onChange.mock.calls.at(-1)?.[0] as AppConfig;
    expect(latest.models?.providers?.gateway.models?.[0].effort?.default).toBe('high');
  });

  it('preserves provider-specific effort metadata while editing known effort fields', async () => {
    const configured = structuredClone(config) as AppConfig;
    configured.models!.providers!.gateway.models![0].effort = {
      levels: ['low', 'high'],
      default: 'high',
      providerScale: 'turbo',
      vendor: { budget: 4096 },
    };
    const { onChange } = await render(vi.fn(), vi.fn(), { config: configured });

    await act(async () =>
      container.querySelector<HTMLButtonElement>('.models-console-card')!.click(),
    );
    const effortEditor = container.querySelector<HTMLElement>('.models-console-effort-editor')!;
    const defaultEffort = effortEditor.querySelector<HTMLSelectElement>(
      '.models-console-effort-default select',
    )!;
    await act(async () => {
      defaultEffort.value = 'low';
      defaultEffort.dispatchEvent(new Event('change', { bubbles: true }));
      await flush();
    });

    let latest = onChange.mock.calls.at(-1)?.[0] as AppConfig;
    expect(latest.models?.providers?.gateway.models?.[0].effort).toEqual({
      levels: ['low', 'high'],
      default: 'low',
      providerScale: 'turbo',
      vendor: { budget: 4096 },
    });

    const low = Array.from(
      effortEditor.querySelectorAll<HTMLInputElement>('.models-console-effort-levels input'),
    ).find((input) => input.parentElement?.textContent?.trim() === 'Low')!;
    await act(async () => {
      low.click();
      await flush();
    });
    latest = onChange.mock.calls.at(-1)?.[0] as AppConfig;
    expect(latest.models?.providers?.gateway.models?.[0].effort).toEqual({
      levels: ['high'],
      default: 'high',
      providerScale: 'turbo',
      vendor: { budget: 4096 },
    });

    const reasoningToggle = Array.from(
      container.querySelectorAll<HTMLInputElement>('.models-console-capability-editor input'),
    ).find((input) => input.parentElement?.textContent?.includes('Reasoning'))!;
    await act(async () => {
      reasoningToggle.click();
      await flush();
    });
    latest = onChange.mock.calls.at(-1)?.[0] as AppConfig;
    expect(latest.models?.providers?.gateway.models?.[0].effort).toEqual({
      levels: ['off'],
      default: 'off',
      providerScale: 'turbo',
      vendor: { budget: 4096 },
    });

    const disabledReasoningToggle = Array.from(
      container.querySelectorAll<HTMLInputElement>('.models-console-capability-editor input'),
    ).find((input) => input.parentElement?.textContent?.includes('Reasoning'))!;
    await act(async () => {
      disabledReasoningToggle.click();
      await flush();
    });
    latest = onChange.mock.calls.at(-1)?.[0] as AppConfig;
    expect(latest.models?.providers?.gateway.models?.[0].effort).toEqual({
      levels: [...THINKING_EFFORT_LEVELS],
      default: 'auto',
      providerScale: 'turbo',
      vendor: { budget: 4096 },
    });
  });

  it('filters cards by capability without mutating configuration', async () => {
    const { onChange } = await render();
    const imageFilter = Array.from(container.querySelectorAll('button')).find(
      (button) => button.textContent?.trim() === 'Image',
    ) as HTMLButtonElement;

    await act(async () => imageFilter.click());

    expect(container.querySelectorAll('.models-console-card')).toHaveLength(1);
    expect(container.querySelector('.models-console-card')?.textContent).toContain('vision-model');
    expect(imageFilter.getAttribute('aria-pressed')).toBe('true');
    expect(onChange).not.toHaveBeenCalled();
  });

  it('shows the provider API type on every model card', async () => {
    await render();
    const cards = Array.from(container.querySelectorAll('.models-console-card'));
    expect(cards).toHaveLength(2);
    expect(cards.every((card) => card.textContent?.includes('OpenAI Completions'))).toBe(true);
    expect(container.querySelector('main')).toBeNull();
  });

  it('uses the shared SVG chevron for model-card navigation', async () => {
    await render();
    const arrows = Array.from(
      container.querySelectorAll<SVGUseElement>('.models-console-card-arrow use'),
    );

    expect(arrows).toHaveLength(2);
    expect(arrows.every((arrow) => arrow.getAttribute('href') === '#icon-chevron-right')).toBe(
      true,
    );
    expect(container.textContent).not.toContain('›');
  });

  it('reports blank-card drafts and resets editor baselines after save or reload', async () => {
    const onChange = vi.fn();
    const onStatus = vi.fn();
    const onDraftDirtyChange = vi.fn();
    const { rerender } = await render(onChange, onStatus, { onDraftDirtyChange });
    const addModel = Array.from(container.querySelectorAll('button')).find(
      (button) => button.textContent?.trim() === 'Add model',
    ) as HTMLButtonElement;

    await act(async () => {
      addModel.click();
      await flush();
    });
    expect(onDraftDirtyChange).toHaveBeenLastCalledWith(true);
    expect(container.querySelectorAll('.models-console-card')).toHaveLength(3);

    await rerender(config, 1);
    expect(onDraftDirtyChange).toHaveBeenLastCalledWith(false);
    expect(container.querySelectorAll('.models-console-card')).toHaveLength(2);

    const firstCard = container.querySelector<HTMLButtonElement>('.models-console-card');
    await act(async () => firstCard?.click());
    const modelId = container.querySelector<HTMLInputElement>(
      '.models-console-inspector input[placeholder="model-id"]',
    );
    await act(async () => {
      setInputValue(modelId!, 'saved-model');
      await flush();
    });
    const savedConfig = onChange.mock.calls.at(-1)?.[0] as AppConfig;
    await rerender(savedConfig, 2);
    expect(container.querySelector('.models-console-json-warning')).toBeNull();
    expect(onStatus.mock.calls.flat().join(' ')).not.toContain('changed outside');

    const reloadedConfig = structuredClone(savedConfig);
    reloadedConfig.models!.providers!.gateway.models![0].id = 'reloaded-model';
    await rerender(reloadedConfig, 3);
    expect(container.querySelector('.models-console-card')?.textContent).toContain(
      'reloaded-model',
    );
  });

  it('moves focus into the mobile inspector and restores the originating card on close', async () => {
    Object.defineProperty(window, 'matchMedia', {
      configurable: true,
      value: vi.fn().mockReturnValue({ matches: true }),
    });
    container.className = 'settings-body';
    container.scrollTop = 640;
    container.scrollLeft = 12;
    await render();
    const card = container.querySelector<HTMLButtonElement>('.models-console-card')!;
    const inspector = container.querySelector<HTMLElement>('.models-console-inspector')!;
    const scrollIntoView = vi.fn(() => {
      container.scrollTop = 0;
      container.scrollLeft = 0;
    });
    inspector.scrollIntoView = scrollIntoView;
    card.focus();

    await act(async () => {
      card.click();
      await new Promise((resolve) => window.setTimeout(resolve, 0));
    });
    expect(document.activeElement).toBe(
      container.querySelector('.models-console-inspector input[placeholder="model-id"]'),
    );
    expect(scrollIntoView).toHaveBeenCalledWith({ block: 'start', inline: 'nearest' });
    expect(container.scrollTop).toBe(0);
    expect(container.scrollLeft).toBe(0);

    const close = container.querySelector<HTMLButtonElement>(
      '.models-console-inspector-head button',
    )!;
    await act(async () => {
      close.click();
      await new Promise((resolve) => window.setTimeout(resolve, 0));
    });
    expect(document.activeElement).toBe(card);
    expect(container.scrollTop).toBe(640);
    expect(container.scrollLeft).toBe(12);
  });

  it('hands focus and scroll state to the inspector when the viewport becomes mobile', async () => {
    let matches = false;
    const listeners = new Set<(event: MediaQueryListEvent) => void>();
    const mediaQuery = {
      get matches() {
        return matches;
      },
      media: '(max-width: 768px)',
      addEventListener: vi.fn((_type: string, listener: (event: MediaQueryListEvent) => void) => {
        listeners.add(listener);
      }),
      removeEventListener: vi.fn(
        (_type: string, listener: (event: MediaQueryListEvent) => void) => {
          listeners.delete(listener);
        },
      ),
    } as unknown as MediaQueryList;
    Object.defineProperty(window, 'matchMedia', {
      configurable: true,
      value: vi.fn().mockReturnValue(mediaQuery),
    });
    container.className = 'settings-body';
    container.scrollTop = 480;
    container.scrollLeft = 16;
    await render();
    const card = container.querySelector<HTMLButtonElement>('.models-console-card')!;
    const inspector = container.querySelector<HTMLElement>('.models-console-inspector')!;
    const scrollIntoView = vi.fn(() => {
      container.scrollTop = 0;
      container.scrollLeft = 0;
    });
    inspector.scrollIntoView = scrollIntoView;
    card.focus();

    await act(async () => {
      card.dispatchEvent(new MouseEvent('click', { bubbles: true, detail: 1 }));
      await flush();
    });
    expect(document.activeElement).toBe(card);

    await act(async () => {
      matches = true;
      for (const listener of listeners) {
        listener({ matches: true, media: mediaQuery.media } as MediaQueryListEvent);
      }
      await flush();
    });
    expect(document.activeElement).toBe(
      container.querySelector('.models-console-inspector input[placeholder="model-id"]'),
    );
    expect(scrollIntoView).toHaveBeenCalledWith({ block: 'start', inline: 'nearest' });
    expect(container.scrollTop).toBe(0);
    expect(container.scrollLeft).toBe(0);

    const close = container.querySelector<HTMLButtonElement>(
      '.models-console-inspector-head button',
    )!;
    await act(async () => {
      close.click();
      await new Promise((resolve) => window.setTimeout(resolve, 0));
    });
    expect(document.activeElement).toBe(card);
    expect(container.scrollTop).toBe(480);
    expect(container.scrollLeft).toBe(16);
  });

  it('moves keyboard activation directly into the desktop model inspector', async () => {
    await render();
    const card = container.querySelector<HTMLButtonElement>('.models-console-card')!;
    card.focus();

    await act(async () => {
      card.dispatchEvent(new MouseEvent('click', { bubbles: true, detail: 0 }));
      await new Promise((resolve) => window.setTimeout(resolve, 0));
    });

    expect(card.getAttribute('aria-controls')).toBe('models-console-inspector');
    expect(document.activeElement).toBe(
      container.querySelector('.models-console-inspector input[placeholder="model-id"]'),
    );
  });

  it('returns focus to model search when editing hides the selected card', async () => {
    await render();
    const reasoningFilter = Array.from(
      container.querySelectorAll<HTMLButtonElement>('.models-console-capabilities button'),
    ).find((button) => button.textContent?.trim() === 'Reasoning');

    await act(async () => {
      reasoningFilter?.click();
      await flush();
    });
    const card = container.querySelector<HTMLButtonElement>('.models-console-card');
    await act(async () => {
      card?.click();
      await flush();
    });
    const reasoningToggle = Array.from(
      container.querySelectorAll<HTMLInputElement>('.models-console-capability-editor input'),
    ).find((input) => input.parentElement?.textContent?.includes('Reasoning'));
    await act(async () => {
      reasoningToggle?.click();
      await flush();
    });
    expect(container.querySelector('.models-console-card')).toBeNull();

    const close = container.querySelector<HTMLButtonElement>(
      '.models-console-inspector-head button',
    );
    await act(async () => {
      close?.click();
      await new Promise((resolve) => window.setTimeout(resolve, 0));
    });

    expect(document.activeElement).toBe(
      container.querySelector<HTMLInputElement>('.models-console-search input'),
    );
  });

  it('creates providers with the in-app form and never calls window.prompt', async () => {
    const promptSpy = vi.spyOn(window, 'prompt');
    const { onChange } = await render();
    const addProvider = Array.from(container.querySelectorAll('button')).find((button) =>
      button.textContent?.includes('Add provider'),
    ) as HTMLButtonElement;

    await act(async () => addProvider.click());
    const dialog = container.querySelector<HTMLFormElement>('.models-console-dialog');
    const nameInput = dialog?.querySelector<HTMLInputElement>('input');
    await act(async () => {
      setInputValue(nameInput!, 'local-ollama');
      dialog?.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }));
      await flush();
    });

    expect(promptSpy).not.toHaveBeenCalled();
    const latest = onChange.mock.calls.at(-1)?.[0] as AppConfig;
    expect(latest.models?.providers?.['local-ollama']).toMatchObject({
      api: 'openai-completions',
      baseUrl: '',
      apiKey: '',
      models: [],
    });
    expect(container.querySelector('.models-console-dialog')).toBeNull();
  });

  it('returns focus to the Add provider trigger when its dialog closes', async () => {
    await render();
    const addProvider = Array.from(container.querySelectorAll('button')).find((button) =>
      button.textContent?.includes('Add provider'),
    ) as HTMLButtonElement;
    addProvider.focus();

    await act(async () => {
      addProvider.click();
      await flush();
    });
    const input = container.querySelector<HTMLInputElement>('.models-console-dialog input');
    expect(document.activeElement).toBe(input);

    await act(async () => {
      input?.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
      await new Promise((resolve) => window.setTimeout(resolve, 0));
    });
    expect(container.querySelector('.models-console-dialog')).toBeNull();
    expect(document.activeElement).toBe(addProvider);
  });

  it('moves focus to the adjacent model card after confirming model deletion', async () => {
    await render();
    const cards = Array.from(container.querySelectorAll<HTMLButtonElement>('.models-console-card'));

    await act(async () => {
      cards[0]?.click();
      await flush();
    });
    const remove = container.querySelector<HTMLButtonElement>('.models-console-delete-button');
    await act(async () => {
      remove?.click();
      await flush();
    });
    const confirm = container.querySelector<HTMLButtonElement>(
      '.models-console-inspector-confirm .btn-danger',
    );
    expect(document.activeElement).toBe(confirm);

    await act(async () => {
      confirm?.click();
      await new Promise((resolve) => window.setTimeout(resolve, 0));
    });

    expect(container.querySelectorAll('.models-console-card')).toHaveLength(1);
    expect(document.activeElement).toBe(cards[1]);
  });

  it('returns focus to the model delete trigger when confirmation is cancelled', async () => {
    await render();
    const card = container.querySelector<HTMLButtonElement>('.models-console-card');

    await act(async () => {
      card?.click();
      await flush();
    });
    const remove = container.querySelector<HTMLButtonElement>('.models-console-delete-button');
    remove?.focus();
    await act(async () => {
      remove?.click();
      await flush();
    });

    const confirm = container.querySelector<HTMLButtonElement>(
      '.models-console-inspector-confirm .btn-danger',
    );
    expect(document.activeElement).toBe(confirm);
    const cancel = container.querySelector<HTMLButtonElement>(
      '.models-console-inspector-confirm .btn-secondary',
    );
    await act(async () => {
      cancel?.click();
      await flush();
    });

    expect(document.activeElement).toBe(
      container.querySelector<HTMLButtonElement>('.models-console-delete-button'),
    );
  });

  it('cancels model deletion with Escape and keeps the event inside the editor', async () => {
    await render();
    const card = container.querySelector<HTMLButtonElement>('.models-console-card');
    await act(async () => {
      card?.click();
      await flush();
    });
    const remove = container.querySelector<HTMLButtonElement>('.models-console-delete-button');
    await act(async () => {
      remove?.click();
      await flush();
    });
    const escapedToDocument = vi.fn();
    document.addEventListener('keydown', escapedToDocument);

    await act(async () => {
      window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
      await flush();
    });
    document.removeEventListener('keydown', escapedToDocument);

    expect(container.querySelector('.models-console-inspector-confirm')).toBeNull();
    expect(container.querySelectorAll('.models-console-card')).toHaveLength(2);
    expect(escapedToDocument).not.toHaveBeenCalled();
    expect(document.activeElement).toBe(
      container.querySelector<HTMLButtonElement>('.models-console-delete-button'),
    );
  });

  it('moves focus to Add provider after deleting the final provider', async () => {
    await render();
    const remove = container.querySelector<HTMLButtonElement>('.models-console-danger-icon');
    await act(async () => {
      remove?.click();
      await flush();
    });
    const confirm = container.querySelector<HTMLButtonElement>(
      '.models-console-confirm .btn-danger',
    );
    expect(document.activeElement).toBe(confirm);

    await act(async () => {
      confirm?.click();
      await new Promise((resolve) => window.setTimeout(resolve, 0));
    });

    const addProvider = container.querySelector<HTMLButtonElement>('.models-console-add-provider');
    expect(container.querySelector('.models-console-provider-tabs')).toBeNull();
    expect(document.activeElement).toBe(addProvider);
  });

  it('keeps Raw JSON disabled until newer visual-form changes are synchronized', async () => {
    await render();
    await act(async () =>
      container.querySelector<HTMLButtonElement>('.models-console-card')?.click(),
    );
    const modelId = container.querySelector<HTMLInputElement>(
      '.models-console-inspector input[placeholder="model-id"]',
    );
    await act(async () => setInputValue(modelId!, 'changed-model'));

    const details = container.querySelector<HTMLDetailsElement>('.models-console-json');
    await act(async () =>
      details?.querySelector('summary')?.dispatchEvent(new MouseEvent('click', { bubbles: true })),
    );
    const applyButton = Array.from(details?.querySelectorAll('button') || []).find(
      (button) => button.textContent?.trim() === 'Apply JSON',
    ) as HTMLButtonElement;
    expect(applyButton.disabled).toBe(true);

    const syncButton = Array.from(details?.querySelectorAll('button') || []).find((button) =>
      button.textContent?.includes('Refresh JSON'),
    ) as HTMLButtonElement;
    await act(async () => syncButton.click());
    expect(details?.querySelector('textarea')?.value).toContain('changed-model');
  });
});
