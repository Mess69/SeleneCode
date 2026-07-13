
template<typename T> class Base {};
template<typename D> class CRTPBase {};
namespace ns { template<typename T> class Tpl {}; }
class Plain {};

class Widget : public Base<int> {};
class App : public CRTPBase<App> {};
class Q : public ns::Tpl<int> {};
class Both : public Base<char>, public Plain {};
