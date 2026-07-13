#include <stdlib.h>
#include "registry.h"

struct Registry {
    int size;
};

struct Registry *registry_new(int size) {
    struct Registry *r = malloc(sizeof(struct Registry));
    r->size = size;
    return r;
}

void registry_free(struct Registry *r) {
    free(r);
}
