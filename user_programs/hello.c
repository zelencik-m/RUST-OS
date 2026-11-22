/* The canonical "program compiled with the OS": prints its arguments.
 *
 *   $ hello a b
 *   hello: argc=3 a b
 */
#include "ulib.h"

int main(int argc, char **argv) {
    printf("hello: argc=%d", argc);
    for (int i = 1; i < argc; i++) {
        printf(" %s", argv[i]);
    }
    printf("\n");
    return 0;
}
