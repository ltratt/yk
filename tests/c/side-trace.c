// Run-time:
//   env-var: YKD_LOG_IR=jit-pre-opt
//   env-var: YKD_LOG=4
//   env-var: YKD_LOG_STATS=-
//   env-var: YKD_SERIALISE_COMPILATION=1
//   stderr:
//     yk-jit-event: start-tracing
//     1
//     yk-jit-event: stop-tracing
//     --- Begin jit-pre-opt ---
//     ...
//     --- End jit-pre-opt ---
//     2
//     yk-jit-event: enter-jit-code
//     3
//     4
//     5
//     6
//     7
//     8
//     9
//     10
//     yk-jit-event: deoptimise
//     12
//     yk-jit-event: enter-jit-code
//     yk-jit-event: deoptimise
//     14
//     yk-jit-event: enter-jit-code
//     yk-jit-event: deoptimise
//     16
//     yk-jit-event: enter-jit-code
//     yk-jit-event: deoptimise
//     18
//     yk-jit-event: enter-jit-code
//     yk-jit-event: deoptimise
//     yk-jit-event: start-side-tracing
//     20
//     yk-jit-event: stop-tracing
//     --- Begin jit-pre-opt ---
//     ...
//     --- End jit-pre-opt ---
//     22
//     yk-jit-event: enter-jit-code
//     yk-jit-event: execute-side-trace
//     24
//     yk-jit-event: execute-side-trace
//     26
//     yk-jit-event: execute-side-trace
//     28
//     yk-jit-event: execute-side-trace
//     30
//     yk-jit-event: deoptimise
//     {
//         ...
//         "trace_executions": 6,
//         ...
//     }
//   stdout:
//     exit

// Testing side tracing functionality. The program immediately compiles a
// trace, which runs for 10 iterations at which point the guard generated by
// the if/else in function `foo` fails. After failing for 5 times, a side trace
// is generated, which is then executed on subsequent guard failures. The
// output shows one shortcoming of the current side-trace implementation: after
// a side-trace ends we deoptimise into the main function just after
// `yk_mt_control_point`. This means after each side-trace execution we have to
// do "one round" in the normal interpreter, instead of running the parent
// trace again. There's likely no immediate need to fix this, since we'll soon
// be using our own codegen which will not have this issue.

#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <yk.h>
#include <yk_testing.h>

int foo(int i) {
  if (i > 10) {
    return 1;
  } else {
    return 2;
  }
}

int main(int argc, char **argv) {
  YkMT *mt = yk_mt_new(NULL);
  yk_mt_hot_threshold_set(mt, 0);
  yk_mt_sidetrace_threshold_set(mt, 4);
  YkLocation loc = yk_location_new();

  int res = 0;
  int i = 20;
  NOOPT_VAL(loc);
  NOOPT_VAL(res);
  NOOPT_VAL(i);
  while (i > 0) {
    yk_mt_control_point(mt, &loc);
    res += foo(i);
    fprintf(stderr, "%d\n", res);
    i--;
  }
  printf("exit");
  NOOPT_VAL(res);
  yk_location_drop(loc);
  yk_mt_shutdown(mt);
  return (EXIT_SUCCESS);
}
