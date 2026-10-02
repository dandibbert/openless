export interface ImeKeyboardEventLike {
  isComposing?: boolean;
  keyCode?: number;
  nativeEvent?: {
    isComposing?: boolean;
    keyCode?: number;
  };
}

/**
 * WebKit/macOS can report IME composition either through isComposing or the
 * legacy keyCode 229 fallback. UI-level Escape handlers must ignore both so
 * Esc can cancel the active IME composition without also closing OpenLess UI.
 */
export function isImeCompositionEvent(event: ImeKeyboardEventLike): boolean {
  const native = event.nativeEvent ?? event;
  return native.isComposing === true || native.keyCode === 229;
}
