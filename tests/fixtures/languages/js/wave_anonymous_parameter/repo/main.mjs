const value = 'outer';
const anonymous = function(value) { return value; };
const named = function inner(value) { return value; };
const arrow = (value) => value;
export const result = [anonymous('left'), named('right'), arrow('middle'), value];
export const move = function(from) { return from; };
