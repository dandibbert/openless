import { isImeCompositionEvent } from './imeKeyboard';

function assertEqual(actual: boolean, expected: boolean, message: string) {
  if (actual !== expected) {
    throw new Error(`${message}: expected ${expected}, got ${actual}`);
  }
}

assertEqual(isImeCompositionEvent({ isComposing: true, keyCode: 27 }), true, 'native isComposing');
assertEqual(isImeCompositionEvent({ isComposing: false, keyCode: 229 }), true, 'native keyCode 229');
assertEqual(
  isImeCompositionEvent({ nativeEvent: { isComposing: true, keyCode: 27 } }),
  true,
  'react nativeEvent isComposing',
);
assertEqual(
  isImeCompositionEvent({ nativeEvent: { isComposing: false, keyCode: 229 } }),
  true,
  'react nativeEvent keyCode 229',
);
assertEqual(isImeCompositionEvent({ isComposing: false, keyCode: 27 }), false, 'native Escape');
assertEqual(
  isImeCompositionEvent({ nativeEvent: { isComposing: false, keyCode: 27 } }),
  false,
  'react Escape',
);

console.log('imeKeyboard tests passed');
