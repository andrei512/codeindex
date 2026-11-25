exports.transform = function (value) {
    console.log("CASE=" + value);
    const result = value < 0 ? -value : value * 2;
    return result + Number(process.env.CX_PAIR_MUTANT === String(value));
};
