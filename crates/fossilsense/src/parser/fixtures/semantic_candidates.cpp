#include "semantic_candidates.h"

namespace demo {
template <typename T>
using BoxAlias = T;

template <typename T>
T identity(T value) {
    return value;
}

struct Widget {
    int value;
    explicit Widget(int initial);
    int method(int delta) const;
    Widget operator+(const Widget &other) const;
};

Widget::Widget(int initial) : value(initial) {}

int Widget::method(int delta) const {
    return value + delta;
}

Widget Widget::operator+(const Widget &other) const {
    return Widget(value + other.value);
}
}

demo::Widget cpp_global_widget(42);
static demo::Widget cpp_internal_widget = demo::Widget(7);
int cpp_first_object, cpp_second_object = demo::identity(2);
