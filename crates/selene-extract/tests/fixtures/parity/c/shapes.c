#include <stdio.h>
#include "shapes.h"

typedef struct Point {
    int x;
    int y;
} Point;

enum Kind {
    KIND_CIRCLE,
    KIND_SQUARE
};

static int area(const Point *p) {
    return p->x * p->y;
}

int describe(const Point *p) {
    int a = area(p);
    printf("area=%d\n", a);
    return a;
}
