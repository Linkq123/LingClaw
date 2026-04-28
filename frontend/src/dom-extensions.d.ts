declare global {
  interface Element {
    hidden: boolean;
    style: CSSStyleDeclaration;
    dataset: DOMStringMap;
    title: string;
    _textNode?: Text;
    _rawText?: string;
    _renderedOffset?: number;
    _markdownRenderedRaw?: string;
    _liveTail?: HTMLElement | null;
    _markdownShouldFollow?: boolean;
    _focusFlashTimer?: number;
    _resetLabelTimer?: number;
  }
}

export {};
