namespace demo {
template <typename T>
inline T clamp_candidate(T value, T low, T high) {
    return value < low ? low : (high < value ? high : value);
}

inline int Widget::method(int delta) const {
    return value + delta;
}
}

inline int qualified_candidate(void) {
    return demo::clamp_candidate(3, 1, 5);
}
