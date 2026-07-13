#include <stdio.h>
#include "app.h"

struct Point {
    int x;
    int y;
};

typedef struct Point Point;

static int add(int a, int b) {
    return a + b;
}

int main(void) {
    Point p = {1, 2};
    printf("%d\n", add(p.x, p.y));
    return 0;
}
