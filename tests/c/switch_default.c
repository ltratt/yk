// Run-time:
//   env-var: YKD_LOG_IR=jit-pre-opt
//   env-var: YKD_SERIALISE_COMPILATION=1
//   env-var: YKD_LOG_JITSTATE=-
//   stderr:
//     jitstate: start-tracing
//     i=3
//     jitstate: stop-tracing
//     --- Begin jit-pre-opt ---
//     ...
//       switch i32 %{{cond}}, label %{{default-bb}} [
//         i32 299, label %guardfail...
//         i32 298, label %guardfail...
//       ]
//
//     guardfail...
//     ...
//     --- End jit-pre-opt ---
//     i=2
//     jitstate: enter-jit-code
//     i=1
//     jitstate: deoptimise
//     ...

// Check that tracing the default arm of a switch works correctly.

#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <yk.h>
#include <yk_testing.h>

int main(int argc, char **argv) {
  YkMT *mt = yk_mt_new(NULL);
  yk_mt_hot_threshold_set(mt, 0);
  YkLocation loc = yk_location_new();
  int i = 3;
  int j = 300;
  NOOPT_VAL(i);
  while (i > 0) {
    yk_mt_control_point(mt, &loc);
    fprintf(stderr, "i=%d\n", i);
    switch (j) {
    case 299:
      exit(1);
    case 298:
      exit(1);
    default:
      i--;
      break;
    }
  }
  yk_location_drop(loc);
  yk_mt_drop(mt);

  return (EXIT_SUCCESS);
}
