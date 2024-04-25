// ignore-if: test $YK_JIT_COMPILER != "yk" -o "$YKB_TRACER" = "swt"
// Run-time:
//   env-var: YKD_LOG_IR=-:aot,jit-pre-opt
//   env-var: YKD_SERIALISE_COMPILATION=1
//   env-var: YKD_LOG_JITSTATE=-
//   stderr:
//     jitstate: start-tracing
//     pos=1
//     jitstate: stop-tracing
//     --- Begin aot ---
//     ...
//     ... = SignExtend ...
//     ...
//     --- End aot ---
//     --- Begin jit-pre-opt ---
//     ...
//     %{{22}}: i64 = SignExtend %{{21}}, i64
//     ...
//     --- End jit-pre-opt ---
//     pos=2
//     jitstate: enter-jit-code
//     pos=3
//     pos=4
//     jitstate: deoptimise

// Check that sign extending with a positive value works.

#include <inttypes.h>
#include <stdio.h>
#include <stdlib.h>
#include <yk.h>
#include <yk_testing.h>

int main(int argc, char **argv) {
  YkMT *mt = yk_mt_new(NULL);
  yk_mt_hot_threshold_set(mt, 0);
  YkLocation loc = yk_location_new();

  int32_t pos = 1;
  NOOPT_VAL(loc);
  while (pos < 5) {
    NOOPT_VAL(pos);
    yk_mt_control_point(mt, &loc);
    fprintf(stderr, "pos=%" PRIi64 "\n",
            (int64_t)pos); // cast causes sign extend.
    pos++;
  }
  yk_location_drop(loc);
  yk_mt_drop(mt);
  return (EXIT_SUCCESS);
}
