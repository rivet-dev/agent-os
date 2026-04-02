/* cexcept.h — WASI-compatible replacement for cexcept exception handling.
 *
 * The original cexcept.h uses setjmp/longjmp which requires the WASM
 * Exception Handling proposal. This simplified version uses a global
 * flag and goto-style flow control that works on all WASM runtimes.
 *
 * Limitations: nested Try/Catch blocks are not reentrant (fine for
 * pngcrush which uses them sequentially). Throw from deeply nested
 * calls will abort the process via exit(1).
 */

#ifndef CEXCEPT_H
#define CEXCEPT_H

#include <stdlib.h>
#include <stdio.h>

#define define_exception_type(etype) \
struct exception_context { \
  int caught; \
  int has_throw; \
  volatile struct { etype etmp; } v; \
}

#define init_exception_context(ec) ((void)((ec)->caught = 0, (ec)->has_throw = 0))

/* Try/Catch: uses a simple flag-based mechanism instead of setjmp/longjmp.
 * Throw sets the flag and the Catch block checks it.
 * NOTE: Throw from a subroutine (not directly in the Try block) will
 * call exit(1) since we can't longjmp back. pngcrush's error handler
 * (pngcrush_cexcept_error) calls Throw, which is invoked from libpng
 * callbacks, so we handle that with exit(). */
#define Try \
  { \
    the_exception_context->caught = 0; \
    the_exception_context->has_throw = 0; \
    {  \
      do

#define exception__catch(action) \
      while (the_exception_context->caught = 0, \
             the_exception_context->caught); \
    } \
    if (the_exception_context->has_throw) { \
      the_exception_context->caught = 1; \
    } \
  } \
  if (!the_exception_context->caught || action) { } \
  else

#define Catch(e) exception__catch(((e) = the_exception_context->v.etmp, 0))
#define Catch_anonymous exception__catch(0)

#define Throw \
  for (;; exit(1)) \
    for (the_exception_context->has_throw = 1 ;; ) \
      the_exception_context->v.etmp =

#endif /* CEXCEPT_H */
