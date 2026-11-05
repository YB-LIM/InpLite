import sys, random
random.seed(1)
out = sys.argv[1]; nsets = int(sys.argv[2]); nn = int(sys.argv[3])
w = open(out, 'w', newline='\r\n')
w.write("*Heading\nflat stress model (HyperMesh-like export)\n*Preprint, echo=NO, model=NO, history=NO\n")
w.write("*NODE\n")
for i in range(1, nn+1):
    w.write(f"{i:8d},{(i%100)*1.5:16.8e},{(i//100%100)*1.5:16.8e},{(i//10000)*1.5:16.8e}\n")
ne = nn // 2
comps = 400
per = ne // comps
eid = 1
for c in range(comps):
    w.write(f"*ELEMENT, TYPE=S4R, ELSET=COMP_{c}\n")
    for k in range(per):
        a = (eid % (nn-200)) + 1
        w.write(f"{eid:8d},{a:8d},{a+1:8d},{a+101:8d},{a+100:8d}\n"); eid += 1
    w.write(f"*SHELL SECTION, ELSET=COMP_{c}, MATERIAL=MAT_{c%50}\n{1.0+c%5*0.2:.2f},5\n")
for s in range(nsets):
    w.write(f"*NSET, NSET=NS_{s}\n")
    ids = sorted(random.sample(range(1, nn), random.randint(1, 24)))
    for j in range(0, len(ids), 16):
        w.write(", ".join(str(x) for x in ids[j:j+16]) + "\n")
    if s % 3 == 0:
        w.write(f"*ELSET, ELSET=ES_{s}, GENERATE\n{s+1}, {s+50}, 1\n")
for m in range(50):
    w.write(f"*MATERIAL, NAME=MAT_{m}\n*DENSITY\n7.85e-09,\n*ELASTIC\n210000., 0.3\n*PLASTIC\n")
    for k in range(8):
        w.write(f"{300+k*20}., {k*0.02}\n")
for k in range(2000):
    a = k*7+1
    w.write(f"*ELEMENT, TYPE=SPRINGA, ELSET=SPR_{k}\n{ne+10+k}, {a}, {a+1}\n*SPRING, ELSET=SPR_{k}\n\n{1000.+k}\n")
w.write("*AMPLITUDE, NAME=RAMP\n0., 0., 1., 1.\n")
w.write("*STEP, NAME=LOAD, NLGEOM=YES\n*STATIC\n0.1, 1., 1e-05, 0.1\n*BOUNDARY\n")
for s in range(0, min(nsets, 3000)):
    w.write(f"NS_{s}, 1, 3\n")
w.write("*CLOAD, AMPLITUDE=RAMP\n")
for k in range(500):
    w.write(f"{k*13+1}, 3, -10.\n")
w.write("*OUTPUT, FIELD, VAR=PRESELECT\n*OUTPUT, HISTORY, VARIABLE=PRESELECT\n*END STEP\n")
w.close()
