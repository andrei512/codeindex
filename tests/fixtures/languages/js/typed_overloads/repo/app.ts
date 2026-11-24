namespace Other { export function save(value: number) { return value; } }
function save(value: string) { return value.length; }
export function run() { return save("x"); }
