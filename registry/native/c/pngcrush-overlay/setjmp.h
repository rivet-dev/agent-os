/* setjmp.h — stub for WASI targets without WASM Exception Handling.
 *
 * pngcrush uses cexcept.h (not setjmp) for its own error handling.
 * However, the bundled libpng's simplified API (png_safe_execute)
 * references setjmp/longjmp directly. Since pngcrush doesn't use
 * the simplified API, these stubs satisfy the linker without
 * requiring the WASM EH proposal.
 *
 * If setjmp is actually called at runtime, we abort.
 */

#ifndef _SETJMP_H
#define _SETJMP_H

#include <stdlib.h>

typedef int jmp_buf[1];

static inline int setjmp(jmp_buf env) {
  (void)env;
  return 0;
}

static inline _Noreturn void longjmp(jmp_buf env, int val) {
  (void)env;
  (void)val;
  abort();
}

#endif /* _SETJMP_H */
