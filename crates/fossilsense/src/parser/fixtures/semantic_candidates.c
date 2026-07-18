#include "semantic_candidates.h"

#define REGISTER_CANDIDATE(name) int registered_##name(void)

extern int c_declared_object;
static int c_internal_object = 1;
int c_first_object, c_second_object = 2;
int (*c_handler)(int);

int c_declared_function(int value);
static int c_internal_function(void) { return c_internal_object; }

REGISTER_CANDIDATE(alpha);

int c_defined_function(int value) {
    return c_declared_function(value) + c_internal_function();
}

int c_malformed_object = };
