struct Base {
    int x;
};

struct Widget {
    int y;
};

class FOO : public Base { int a; };
struct BAR : public Base { int b; };
class Multi : public Base, public Widget { int c; };
