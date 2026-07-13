
struct Widget { void draw(); };
class Factory { public: static Widget create(); };
Widget Factory::create() { return Widget(); }
void doNothing() {}
