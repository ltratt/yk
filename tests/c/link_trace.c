// Run-time:
//   env-var: YKD_LOG=4
//   env-var: YKD_LOG_IR=-:jit-asm
//   env-var: YKD_SERIALISE_COMPILATION=1
//   stderr:
//     exit

#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <yk.h>
#include <yk_testing.h>

static int counter = 0;

__attribute__((yk_outline)) int next() { return counter++; }
__attribute__((yk_outline)) int invis_plus_one() { return 1; }

int main(int argc, char **argv) {
  YkMT *mt = yk_mt_new(NULL);
  yk_mt_hot_threshold_set(mt, 0);
  yk_mt_sidetrace_threshold_set(mt, 1);
  YkLocation loc1 = yk_location_new();
  YkLocation loc2 = yk_location_new();

  int res = 0;
  int x = 11;
  int y = 0;
  int *z = &y;
  NOOPT_VAL(loc1);
  NOOPT_VAL(loc2);
  NOOPT_VAL(res);
  NOOPT_VAL(x);
  NOOPT_VAL(y);
  NOOPT_VAL(z);
  int a = next();
  int b = next();
  int c = next();
  int d = next();
  int e = next();
  int f = next();
  int g = next();
  while (x > 0) {
    a += next();
    b += next();
    c += next();
    d += next();
    e += next();
    f += next();
    g += next();
    *z = x;
    z += invis_plus_one();
    YkLocation *loc;
    if (x > 9 || x == 8 || x == 4) {
      loc = &loc1;
    } else {
      loc = &loc2;
    }
    yk_mt_control_point(mt, loc);
    fprintf(stderr, "%d %d %d %d\n", x, a, b, c);
    fprintf(stderr, "%d %d %d %d\n", d, e, f, g);
    fprintf(stderr, "%d\n", y);
    z -= invis_plus_one();
    x--;
  }
  fprintf(stderr, "\n");
  yk_location_drop(loc1);
  yk_location_drop(loc2);
  yk_mt_shutdown(mt);
  fprintf(stderr, "exit");
  return (EXIT_SUCCESS);
}
