void marker() {}
void shadow(void Function() marker) { marker(); }
void positive() { marker(); }
void scoped() { if (true) { final marker = 1; } marker(); }
