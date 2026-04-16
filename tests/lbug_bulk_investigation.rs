// Investigation: what bulk-insert patterns does lbug 0.15.3 actually support?
// Each test is a compile-and-run probe, not a product test.

use lbug::{Connection, Database, LogicalType, SystemConfig, Value};

fn tmpdb(name: &str) -> (tempfile::TempDir, Database) {
    let dir = tempfile::Builder::new().prefix(name).tempdir().unwrap();
    let db = Database::new(dir.path().join("testdb"), SystemConfig::default()).unwrap();
    (dir, db)
}

#[test]
fn probe_1_prepared_statement_with_single_params() {
    // Baseline: prepare + execute with scalar params. Expected to work.
    let (_dir, db) = tmpdb("probe1");
    let conn = Connection::new(&db).unwrap();
    conn.query("CREATE NODE TABLE P(id STRING, age INT64, PRIMARY KEY(id));").unwrap();
    let mut stmt = conn
        .prepare("CREATE (:P {id: $id, age: $age});")
        .expect("prepare must succeed");
    for i in 0..3i64 {
        conn.execute(
            &mut stmt,
            vec![
                ("id", Value::String(format!("p{i}"))),
                ("age", Value::Int64(i)),
            ],
        )
        .expect("execute must succeed");
    }
    let mut r = conn.query("MATCH (p:P) RETURN count(p);").unwrap();
    let row = r.next().unwrap();
    assert_eq!(row[0], Value::Int64(3));
}

#[test]
fn probe_2_unwind_list_of_structs_to_create_nodes() {
    // THE KEY TEST: can we pass a list-of-structs as a parameter and UNWIND it
    // to create many nodes in one execute() call?
    let (_dir, db) = tmpdb("probe2");
    let conn = Connection::new(&db).unwrap();
    conn.query("CREATE NODE TABLE P(id STRING, age INT64, PRIMARY KEY(id));").unwrap();

    let mut stmt = match conn.prepare(
        "UNWIND $rows AS row CREATE (:P {id: row.id, age: row.age});",
    ) {
        Ok(s) => s,
        Err(e) => panic!("UNWIND $rows AS row ... prepare FAILED: {e}"),
    };

    // Build a Value::List of Value::Struct rows. Child type must be Struct
    // with matching field names + types for lbug's binder to accept it.
    let row_type = LogicalType::Struct {
        fields: vec![
            ("id".to_string(), LogicalType::String),
            ("age".to_string(), LogicalType::Int64),
        ],
    };
    let mut rows: Vec<Value> = Vec::new();
    for i in 0..100i64 {
        rows.push(Value::Struct(vec![
            ("id".to_string(), Value::String(format!("u{i}"))),
            ("age".to_string(), Value::Int64(i)),
        ]));
    }
    let list = Value::List(row_type, rows);

    match conn.execute(&mut stmt, vec![("rows", list)]) {
        Ok(_) => {
            let mut r = conn.query("MATCH (p:P) RETURN count(p);").unwrap();
            let row = r.next().unwrap();
            assert_eq!(row[0], Value::Int64(100), "UNWIND list-of-structs WORKED");
        }
        Err(e) => panic!("UNWIND execute FAILED with LogicalType::Any list: {e}"),
    };
}

#[test]
fn probe_3_unwind_parallel_lists_for_edges() {
    // If struct lists don't work, try parallel primitive lists.
    let (_dir, db) = tmpdb("probe3");
    let conn = Connection::new(&db).unwrap();
    conn.query("CREATE NODE TABLE P(id STRING, PRIMARY KEY(id));").unwrap();
    conn.query("CREATE REL TABLE R(FROM P TO P);").unwrap();
    for i in 0..10 {
        conn.query(&format!("CREATE (:P {{id: 'n{i}'}});")).unwrap();
    }

    // Try: UNWIND $ids AS id MATCH (p:P) WHERE p.id = id RETURN p;
    let mut stmt = conn
        .prepare("UNWIND $ids AS id MATCH (p:P) WHERE p.id = id RETURN p.id;")
        .expect("prepare UNWIND with primitive list");

    let ids: Vec<Value> = (0..10).map(|i| Value::String(format!("n{i}"))).collect();
    let list = Value::List(LogicalType::String, ids);
    let mut r = conn.execute(&mut stmt, vec![("ids", list)]).expect("execute");
    let mut count = 0;
    while let Some(_) = r.next() { count += 1; }
    assert_eq!(count, 10, "UNWIND primitive list WORKED");
}

#[test]
fn probe_4_unwind_for_edges_with_match_and_create() {
    // The real target: one execute() creates N edges. This is what the prior
    // engineer claimed failed. Let's verify exactly what syntax.
    let (_dir, db) = tmpdb("probe4");
    let conn = Connection::new(&db).unwrap();
    conn.query("CREATE NODE TABLE P(id STRING, PRIMARY KEY(id));").unwrap();
    conn.query("CREATE REL TABLE R(FROM P TO P);").unwrap();
    for i in 0..20 {
        conn.query(&format!("CREATE (:P {{id: 'n{i}'}});")).unwrap();
    }

    // Candidate A: UNWIND over struct rows, MATCH both endpoints, CREATE edge.
    let mut stmt = match conn.prepare(
        "UNWIND $rows AS row \
         MATCH (a:P), (b:P) \
         WHERE a.id = row.from AND b.id = row.to \
         CREATE (a)-[:R]->(b);",
    ) {
        Ok(s) => s,
        Err(e) => panic!("UNWIND edge prepare FAILED: {e}"),
    };

    let row_type = LogicalType::Struct {
        fields: vec![
            ("from".to_string(), LogicalType::String),
            ("to".to_string(), LogicalType::String),
        ],
    };
    let mut rows: Vec<Value> = Vec::new();
    for i in 0..19 {
        rows.push(Value::Struct(vec![
            ("from".to_string(), Value::String(format!("n{i}"))),
            ("to".to_string(), Value::String(format!("n{}", i + 1))),
        ]));
    }
    let list = Value::List(row_type, rows);
    match conn.execute(&mut stmt, vec![("rows", list)]) {
        Ok(_) => {
            let mut r = conn.query("MATCH ()-[r:R]->() RETURN count(r);").unwrap();
            let row = r.next().unwrap();
            assert_eq!(row[0], Value::Int64(19), "UNWIND edge-bulk WORKED");
        }
        Err(e) => panic!("UNWIND edge execute FAILED: {e}"),
    };
}

#[test]
fn probe_5_begin_transaction() {
    // Does an explicit transaction statement work?
    let (_dir, db) = tmpdb("probe5");
    let conn = Connection::new(&db).unwrap();
    conn.query("CREATE NODE TABLE P(id STRING, PRIMARY KEY(id));").unwrap();

    match conn.query("BEGIN TRANSACTION;") {
        Ok(_) => println!("BEGIN TRANSACTION: OK"),
        Err(e) => println!("BEGIN TRANSACTION: FAILED: {e}"),
    }
    match conn.query("CREATE (:P {id: 't1'});") {
        Ok(_) => println!("CREATE inside tx: OK"),
        Err(e) => println!("CREATE inside tx: FAILED: {e}"),
    }
    match conn.query("COMMIT;") {
        Ok(_) => println!("COMMIT: OK"),
        Err(e) => println!("COMMIT: FAILED: {e}"),
    };
}

#[test]
fn probe_6_multi_statement_single_call() {
    // The connection.rs test `test_multiple_statement_query` already proves
    // multi-statement works. Verify here with 500 CREATEs in one query string.
    let (_dir, db) = tmpdb("probe6");
    let conn = Connection::new(&db).unwrap();
    conn.query("CREATE NODE TABLE P(id STRING, PRIMARY KEY(id));").unwrap();
    let mut big = String::new();
    for i in 0..500 {
        big.push_str(&format!("CREATE (:P {{id: 'm{i}'}});\n"));
    }
    let t0 = std::time::Instant::now();
    conn.query(&big).expect("500 CREATEs in one query");
    let dt = t0.elapsed();
    eprintln!("500 CREATEs concatenated: {:?}", dt);
    let mut r = conn.query("MATCH (p:P) RETURN count(p);").unwrap();
    assert_eq!(r.next().unwrap()[0], Value::Int64(500));
}

#[test]
fn probe_7_prepared_edge_loop_vs_unprepared() {
    // Even if UNWIND fails, prepared-statement loop for edges should beat
    // raw-string CREATE+MATCH by amortizing planning.
    let (_dir, db) = tmpdb("probe7");
    let conn = Connection::new(&db).unwrap();
    conn.query("CREATE NODE TABLE P(id STRING, PRIMARY KEY(id));").unwrap();
    conn.query("CREATE REL TABLE R(FROM P TO P);").unwrap();
    for i in 0..200 {
        conn.query(&format!("CREATE (:P {{id: 'e{i}'}});")).unwrap();
    }

    // Unprepared: build cypher per edge.
    let t0 = std::time::Instant::now();
    for i in 0..199 {
        let cy = format!(
            "MATCH (a:P), (b:P) WHERE a.id = 'e{i}' AND b.id = 'e{}' CREATE (a)-[:R]->(b);",
            i + 1
        );
        conn.query(&cy).unwrap();
    }
    let raw_dt = t0.elapsed();

    // Clean edges.
    conn.query("MATCH ()-[r:R]->() DELETE r;").unwrap();

    // Prepared loop.
    let mut stmt = conn.prepare(
        "MATCH (a:P), (b:P) WHERE a.id = $from AND b.id = $to CREATE (a)-[:R]->(b);",
    ).expect("prepare edge match+create");
    let t1 = std::time::Instant::now();
    for i in 0..199 {
        conn.execute(&mut stmt, vec![
            ("from", Value::String(format!("e{i}"))),
            ("to", Value::String(format!("e{}", i + 1))),
        ]).unwrap();
    }
    let prep_dt = t1.elapsed();

    eprintln!("199 edges raw string:  {:?}", raw_dt);
    eprintln!("199 edges prepared:    {:?}", prep_dt);
    eprintln!("speedup:               {:.2}x", raw_dt.as_secs_f64() / prep_dt.as_secs_f64());
}

#[test]
fn probe_8_prepared_in_explicit_transaction() {
    // Wrap prepared-loop edge inserts in BEGIN/COMMIT.
    let (_dir, db) = tmpdb("probe8");
    let conn = Connection::new(&db).unwrap();
    conn.query("CREATE NODE TABLE P(id STRING, PRIMARY KEY(id));").unwrap();
    conn.query("CREATE REL TABLE R(FROM P TO P);").unwrap();
    for i in 0..200 {
        conn.query(&format!("CREATE (:P {{id: 'tx{i}'}});")).unwrap();
    }

    let mut stmt = conn.prepare(
        "MATCH (a:P), (b:P) WHERE a.id = $from AND b.id = $to CREATE (a)-[:R]->(b);",
    ).unwrap();

    let t0 = std::time::Instant::now();
    conn.query("BEGIN TRANSACTION;").unwrap();
    for i in 0..199 {
        conn.execute(&mut stmt, vec![
            ("from", Value::String(format!("tx{i}"))),
            ("to", Value::String(format!("tx{}", i + 1))),
        ]).unwrap();
    }
    conn.query("COMMIT;").unwrap();
    let dt = t0.elapsed();
    eprintln!("199 edges BEGIN/prepared/COMMIT: {:?}  ({:.2} ms/edge)",
        dt, dt.as_secs_f64() * 1000.0 / 199.0);
    let mut r = conn.query("MATCH ()-[r:R]->() RETURN count(r);").unwrap();
    assert_eq!(r.next().unwrap()[0], Value::Int64(199));
}

#[test]
fn probe_9_unwind_edges_in_one_execute() {
    // Bulk edge insert via UNWIND list-of-structs, now that we know the
    // child type must be Struct.
    let (_dir, db) = tmpdb("probe9");
    let conn = Connection::new(&db).unwrap();
    conn.query("CREATE NODE TABLE P(id STRING, PRIMARY KEY(id));").unwrap();
    conn.query("CREATE REL TABLE R(FROM P TO P);").unwrap();
    for i in 0..200 {
        conn.query(&format!("CREATE (:P {{id: 'u{i}'}});")).unwrap();
    }

    let mut stmt = conn.prepare(
        "UNWIND $rows AS row \
         MATCH (a:P), (b:P) \
         WHERE a.id = row.from AND b.id = row.to \
         CREATE (a)-[:R]->(b);",
    ).expect("prepare UNWIND edge");

    let row_type = LogicalType::Struct {
        fields: vec![
            ("from".to_string(), LogicalType::String),
            ("to".to_string(), LogicalType::String),
        ],
    };
    let rows: Vec<Value> = (0..199)
        .map(|i| Value::Struct(vec![
            ("from".to_string(), Value::String(format!("u{i}"))),
            ("to".to_string(), Value::String(format!("u{}", i + 1))),
        ]))
        .collect();
    let list = Value::List(row_type, rows);

    let t0 = std::time::Instant::now();
    conn.execute(&mut stmt, vec![("rows", list)]).expect("execute UNWIND edge");
    let dt = t0.elapsed();
    eprintln!("199 edges UNWIND-single-call: {:?}  ({:.3} ms/edge)",
        dt, dt.as_secs_f64() * 1000.0 / 199.0);
    let mut r = conn.query("MATCH ()-[r:R]->() RETURN count(r);").unwrap();
    assert_eq!(r.next().unwrap()[0], Value::Int64(199));
}

