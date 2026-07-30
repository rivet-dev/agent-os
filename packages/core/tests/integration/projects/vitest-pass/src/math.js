export function add(left, right) {
  return left + right;
}

export async function delayedValue(value) {
  await Promise.resolve();
  return value;
}
