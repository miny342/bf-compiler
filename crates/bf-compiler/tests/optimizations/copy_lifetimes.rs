//! Last-use transport must preserve snapshots through control and call edges.
use bf_compiler as bfc;

#[test]
fn copy_lifetimes_preserve_branches_loops_calls_portals_and_global_returns() {
    let helpers = "struct Pair {cell x;cell y;}
        cell g; Pair global_pair;
        cell get(){return g;}
        Pair get_pair(){return global_pair;}
        Pair change(Pair p){p.x+=1;p.y-=1;return p;}
        Pair combine(Pair a,Pair b){a.x+=b.x;a.y+=b.y;return a;}
        cell sum(cell a,cell b){return a+b;}
        cell recur(cell d,cell x){if(d==0){return x;}return recur(d-1,x);}";
    let cases = [
        "cell saved=b; if(c){a+=saved;}else{a-=saved;} output(a);output(saved);",
        "cell i=3; while(i){cell saved=b; a+=saved;output(saved);i-=1;}output(a);output(b);",
        "output(sum(b,b));output(b);output(recur(3,a));output(a);",
        "Pair p;p.x=a;p.y=b;Pair q=p;p.x=c;output(q.x);output(q.y);output(p.x);",
        "Pair p;p.x=a;p.y=b;Pair q=combine(p,p);output(q.x);output(q.y);output(p.x);",
        "Pair p;p.x=a;p.y=b;Pair q=change(p);output(q.x);output(q.y);output(p.x);output(p.y);",
        "Pair[2] p;p[0].x=a;p[0].y=b;p[1].x=c;Pair q=p[0];output(q.x);output(q.y);output(p[1].x);",
        "cell[2] p;p[0]=a;p[1]=b;cell i=c!=0;output(p[i]);p[i]=c;output(p[0]);output(p[1]);",
        "g=b;global_pair.x=a;global_pair.y=c;output(get());output(g);Pair p=get_pair();output(p.x);output(p.y);output(global_pair.x);output(global_pair.y);",
    ];
    let mut input = Vec::new();
    for b in 0..=255u8 {
        for (a, c) in [(0, 0), (1, 1), (15, 16), (127, 128), (254, 255), (255, 0)] {
            input.extend([1, a, b, c]);
        }
    }
    input.push(0);
    for case in cases {
        let source = format!(
            "{helpers} void main(){{while(input()){{cell a=input();cell b=input();cell c=input();{case}}}}}"
        );
        for inline_functions in [false, true] {
            let (p, _) = bfc::lower_source_with_options(
                &source,
                bfc::ContinuationOptimizationOptions {
                    inline_functions,
                    ..Default::default()
                },
            )
            .unwrap();
            let mut expected = Vec::new();
            bfc::run_continuations_with_io(
                &p,
                &mut input.as_slice(),
                &mut expected,
                Default::default(),
                |_| {},
            )
            .unwrap();
            for options in [
                bfc::AbiCodegenOptions::default(),
                bfc::AbiCodegenOptions {
                    nibble_transfer: true,
                    inplace_compare: true,
                    anchor_bank: true,
                    ..Default::default()
                },
                bfc::AbiCodegenOptions {
                    static_frames: true,
                    ..Default::default()
                },
            ] {
                let bf = bfc::optimize_bf(
                    &bfc::lower_continuations_with_codegen_options(&p, options).unwrap(),
                )
                .to_source();
                assert_eq!(
                    bf_interpreter::run(bf.as_bytes(), &input).unwrap(),
                    expected,
                    "case={case} inline={inline_functions} options={options:?}"
                );
            }
        }
    }
}
