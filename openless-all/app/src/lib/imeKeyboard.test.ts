import assert from 'node:assert/strict';
import { isImeCompositionEvent } from './imeKeyboard';

assert.equal(isImeCompositionEvent({ isComposing: true, keyCode: 27 }), true);
assert.equal(isImeCompositionEvent({ isComposing: false, keyCode: 229 }), true);
assert.equal(
  isImeCompositionEvent({ nativeEvent: { isComposing: true, keyCode: 27 } }),
  true,
);
assert.equal(
  isImeCompositionEvent({ nativeEvent: { isComposing: false, keyCode: 229 } }),
  true,
);
assert.equal(isImeCompositionEvent({ isComposing: false, keyCode: 27 }), false);
assert.equal(
  isImeCompositionEvent({ nativeEvent: { isComposing: false, keyCode: 27 } }),
  false,
);

console.log('imeKeyboard tests passed');
