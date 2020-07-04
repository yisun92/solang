use super::{build_solidity, first_error};
use solang::{parse_and_resolve, Target};

#[test]
fn abi_decode() {
    let ns = parse_and_resolve(
        r#"
        contract printer {
            function test() public {
                (int a) = abi.decode(hex"00", feh);
            }
        }"#,
        Target::Substrate,
    );

    assert_eq!(first_error(ns.diagnostics), "type ‘feh’ not found");

    let ns = parse_and_resolve(
        r#"
        contract printer {
            function test() public {
                (int a) = abi.decode(hex"00", (int storage));
            }
        }"#,
        Target::Substrate,
    );

    assert_eq!(
        first_error(ns.diagnostics),
        "storage modifier ‘storage’ not allowed"
    );

    let ns = parse_and_resolve(
        r#"
        contract printer {
            function test() public {
                (int a) = abi.decode(hex"00", (int feh));
            }
        }"#,
        Target::Substrate,
    );

    assert_eq!(
        first_error(ns.diagnostics),
        "unexpected identifier ‘feh’ in type"
    );

    let ns = parse_and_resolve(
        r#"
        contract printer {
            function test() public {
                (int a) = abi.decode(hex"00", (int,));
            }
        }"#,
        Target::Substrate,
    );

    assert_eq!(first_error(ns.diagnostics), "missing type");

    let ns = parse_and_resolve(
        r#"
        contract printer {
            function test() public {
                (int a) = abi.decode(hex"00", (int,mapping(uint[] => address)));
            }
        }"#,
        Target::Substrate,
    );

    assert_eq!(
        first_error(ns.diagnostics),
        "key of mapping cannot be array type"
    );

    let mut runtime = build_solidity(
        r##"
        contract bar {
            function test() public {
                (int16 a, bool b) = abi.decode(hex"7f0001", (int16, bool));

                assert(a == 127);
                assert(b == true);
            }
        }"##,
    );

    runtime.function("test", Vec::new());

    let mut runtime = build_solidity(
        r##"
        contract bar {
            function test() public {
                uint8 a = abi.decode(hex"40", (uint8));

                assert(a == 64);
            }
        }"##,
    );

    runtime.function("test", Vec::new());
}

#[test]
fn abi_encode() {
    let mut runtime = build_solidity(
        r##"
        struct s {
            int32 f1;
            uint8 f2;
            string f3;
            uint16[2] f4;
        }

        contract bar {
            function test() public {
                uint16 a = 0xfd01;
                assert(abi.encode(a) == hex"01fd");
                uint32 b = 0xaabbccdd;
                assert(abi.encode(true, b, false) == hex"01ddccbbaa00");
            }

            function test2() public {
                string b = "foobar";
                assert(abi.encode(b) == hex"18666f6f626172");

                assert(abi.encode("foobar") == hex"18666f6f626172");
            }

            function test3() public {
                s x = s({ f1: 511, f2: 0xf7, f3: "testie", f4: [ uint16(4), 5 ] });

                assert(abi.encode(x) == hex"ff010000f71874657374696504000500");
            }
        }"##,
    );

    runtime.function("test", Vec::new());
    runtime.heap_verify();

    runtime.function("test2", Vec::new());
    runtime.heap_verify();

    runtime.function("test3", Vec::new());
    runtime.heap_verify();
}

#[test]
fn abi_encode_packed() {
    let mut runtime = build_solidity(
        r##"
        struct s {
            int32 f1;
            uint8 f2;
            string f3;
            uint16[2] f4;
        }

        contract bar {
            function test() public {
                uint16 a = 0xfd01;
                assert(abi.encodePacked(a) == hex"01fd");
                uint32 b = 0xaabbccdd;
                assert(abi.encodePacked(true, b, false) == hex"01ddccbbaa00");
            }

            function test2() public {
                string b = "foobar";
                assert(abi.encodePacked(b) == "foobar");

                assert(abi.encodePacked("foobar") == "foobar");
                assert(abi.encodePacked("foo", "bar") == "foobar");
            }

            function test3() public {
                s x = s({ f1: 511, f2: 0xf7, f3: "testie", f4: [ uint16(4), 5 ] });

                assert(abi.encodePacked(x) == hex"ff010000f774657374696504000500");
            }
        }"##,
    );

    runtime.function("test", Vec::new());

    runtime.function("test2", Vec::new());

    runtime.function("test3", Vec::new());
}

#[test]
fn abi_encode_with_selector() {
    let ns = parse_and_resolve(
        r#"
        contract printer {
            function test() public {
                bytes x = abi.encodeWithSelector();
            }
        }"#,
        Target::Substrate,
    );

    assert_eq!(
        first_error(ns.diagnostics),
        "function requires one ‘bytes4’ selector argument"
    );

    let mut runtime = build_solidity(
        r##"
        contract bar {
            function test1() public {
                uint16 a = 0xfd01;
                assert(abi.encodeWithSelector(hex"44332211", a) == hex"4433221101fd");
                uint32 b = 0xaabbccdd;
                assert(abi.encodeWithSelector(hex"aabbccdd", true, b, false) == hex"aabbccdd01ddccbbaa00");

                assert(abi.encodeWithSelector(hex"aabbccdd") == hex"aabbccdd");
            }

            function test2() public {
                uint8[] arr = new uint8[](3);

                arr[0] = 0xfe;
                arr[1] = 0xfc;
                arr[2] = 0xf8;

                assert(abi.encodeWithSelector(hex"01020304", arr) == hex"010203040cfefcf8");
            }
        }"##,
    );

    runtime.function("test1", Vec::new());

    runtime.function("test2", Vec::new());
}

#[test]
fn abi_encode_with_signature() {
    let ns = parse_and_resolve(
        r#"
        contract printer {
            function test() public {
                bytes x = abi.encodeWithSignature();
            }
        }"#,
        Target::Substrate,
    );

    assert_eq!(
        first_error(ns.diagnostics),
        "function requires one ‘string’ signature argument"
    );

    let mut runtime = build_solidity(
        r##"
        contract bar {
            string bla = "Hello, World!";

            function test1() public {
                assert(keccak256("Hello, World!") == hex"acaf3289d7b601cbd114fb36c4d29c85bbfd5e133f14cb355c3fd8d99367964f");

                assert(abi.encodeWithSignature("Hello, World!") == hex"acaf3289");
                assert(abi.encodeWithSignature(bla) == hex"acaf3289");
            }

            function test2() public {
                uint8[] arr = new uint8[](3);

                arr[0] = 0xfe;
                arr[1] = 0xfc;
                arr[2] = 0xf8;

                assert(abi.encodeWithSelector(hex"01020304", arr) == hex"010203040cfefcf8");
            }
        }"##,
    );

    runtime.constructor(0, Vec::new());
    runtime.function("test1", Vec::new());
    runtime.function("test2", Vec::new());
}

#[test]
fn call() {
    let ns = parse_and_resolve(
        r#"
        contract main {
            function test() public {
                address x = address(0);

                x.delegatecall(hex"1222");
            }
        }"#,
        Target::Substrate,
    );

    assert_eq!(
        first_error(ns.diagnostics),
        "method ‘delegatecall’ does not exist"
    );

    let ns = parse_and_resolve(
        r#"
        contract main {
            function test() public {
                address x = address(0);

                x.staticcall(hex"1222");
            }
        }"#,
        Target::Substrate,
    );

    assert_eq!(
        first_error(ns.diagnostics),
        "method ‘staticcall’ does not exist"
    );

    let ns = parse_and_resolve(
        r#"
        contract superior {
            function test() public {
                inferior i = new inferior();

                bytes x = address(i).call(hex"1222");
            }
        }

        contract inferior {
            function baa() public {
                print("Baa!");
            }
        }"#,
        Target::Substrate,
    );

    assert_eq!(
        first_error(ns.diagnostics),
        "destucturing statement needed for function that returns multiple values"
    );

    let ns = parse_and_resolve(
        r#"
        contract superior {
            function test() public {
                inferior i = new inferior();

            (bytes x, bool y) = address(i).call(hex"1222");
            }
        }

        contract inferior {
            function baa() public {
                print("Baa!");
            }
        }"#,
        Target::Substrate,
    );

    assert_eq!(
        first_error(ns.diagnostics),
        "conversion from bool to bytes not possible"
    );

    let mut runtime = build_solidity(
        r##"
        contract superior {
            function test1() public {
                inferior i = new inferior();

                i.test1();

                assert(keccak256("test1()") == hex"6b59084dfb7dcf1c687dd12ad5778be120c9121b21ef90a32ff73565a36c9cd3");

                bytes bs;
                bool success;

                (success, bs) = address(i).call(hex"6b59084d");

                assert(success == true);
                assert(bs == hex"");
            }

            function test2() public {
                inferior i = new inferior();

                assert(i.test2(257) == 256);

                assert(keccak256("test2(uint64)") == hex"296dacf0801def8823747fbd751fbc1444af573e88de40d29c4d01f6013bf095");

                bytes bs;
                bool success;

                (success, bs) = address(i).call(hex"296dacf0_0101_0000__0000_0000");

                assert(success == true);
                assert(bs == hex"0001_0000__0000_0000");
            }
        }

        contract inferior {
            function test1() public {
                print("Baa!");
            }

            function test2(uint64 x) public returns (uint64) {
                return x ^ 1;
            }
        }"##,
    );

    runtime.function("test1", Vec::new());
    runtime.function("test2", Vec::new());

    let mut runtime = build_solidity(
        r##"
        contract superior {
            function test1() public {
                inferior i = new inferior();

                assert(keccak256("test1()") == hex"6b59084dfb7dcf1c687dd12ad5778be120c9121b21ef90a32ff73565a36c9cd3");

                bytes bs;
                bool success;

                (success, bs) = address(i).call(abi.encodeWithSelector(hex"6b59084d"));

                assert(success == true);
                assert(bs == hex"");

                (success, bs) = address(i).call(abi.encodeWithSignature("test1()"));

                assert(success == true);
                assert(bs == hex"");
            }

            function test2() public {
                inferior i = new inferior();
                assert(keccak256("test2(uint64)") == hex"296dacf0801def8823747fbd751fbc1444af573e88de40d29c4d01f6013bf095");

                bytes bs;
                bool success;

                (success, bs) = address(i).call(abi.encodeWithSelector(hex"296dacf0", uint64(257)));

                assert(success == true);
                
                assert(abi.decode(bs, (uint64)) == 256);


                (success, bs) = address(i).call(abi.encodeWithSignature("test2(uint64)", uint64(0xfeec)));

                assert(success == true);
                
                assert(abi.decode(bs, (uint64)) == 0xfeed);
            }
        }

        contract inferior {
            function test1() public {
                print("Baa!");
            }

            function test2(uint64 x) public returns (uint64) {
                return x ^ 1;
            }
        }"##,
    );

    runtime.function("test1", Vec::new());
    runtime.function("test2", Vec::new());
}