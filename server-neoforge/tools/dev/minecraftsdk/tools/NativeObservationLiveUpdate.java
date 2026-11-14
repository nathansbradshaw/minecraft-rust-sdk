package dev.minecraftsdk.tools;

import com.sun.tools.attach.VirtualMachine;
import java.lang.instrument.ClassDefinition;
import java.lang.instrument.Instrumentation;
import java.nio.file.Files;
import java.nio.file.LinkOption;
import java.nio.file.Path;
import java.nio.file.StandardOpenOption;
import java.nio.ByteBuffer;
import java.nio.channels.FileChannel;
import java.security.MessageDigest;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.HashSet;
import java.util.HexFormat;
import java.util.Map;
import java.util.Properties;
import java.util.Set;

/** Fixed-target observer update. Gameplay, connections and server lifecycle are untouched. */
public final class NativeObservationLiveUpdate {
    private static final String TARGET="dev.minecraftsdk.NativeObservation";
    private static final String RESOURCE="NativeObservation.class";
    private static final int LIMIT=4*1024*1024;
    private static final Map<Class<?>,String> SUBMITTED=new HashMap<>();
    private static final Set<String> CLAIMED=new HashSet<>();
    private static final Set<Class<?>> UNCERTAIN=new HashSet<>();
    private NativeObservationLiveUpdate() {}

    public static void main(String[] args)throws Exception {
        if(args.length==2&&args[0].equals("--process-start")) {
            System.out.println(ProcessHandle.of(Long.parseLong(args[1])).orElseThrow().info().startInstant().orElseThrow());
            return;
        }
        if(args.length!=3)throw new IllegalArgumentException("PID ABSOLUTE_AGENT_JAR ABSOLUTE_PLAN required");
        Path jar=Path.of(args[1]),plan=Path.of(args[2]);
        if(!jar.isAbsolute()||!plan.isAbsolute())throw new IllegalArgumentException("Absolute paths required");
        var vm=VirtualMachine.attach(args[0]);
        try {vm.loadAgent(jar.toString(),plan.toString());}finally {vm.detach();}
    }

    public static synchronized void agentmain(String argument,Instrumentation instrumentation)throws Exception {
        var plan=new Properties();
        byte[] planBytes=regular(Path.of(argument),16384);
        try(var input=new java.io.ByteArrayInputStream(planBytes)){plan.load(input);}
        String mode=plan.getProperty("mode","inspect");
        boolean apply=mode.equals("apply");
        if(!apply&&!mode.equals("inspect"))throw new IllegalArgumentException("Closed inspect/apply mode required");
        var required=new HashSet<>(Set.of("pid","started","resource_sha256","replacement","replacement_sha256","receipt","operation_id"));
        if(apply)required.addAll(Set.of("expected_loader_id","expected_code_source","expected_schema_sha256","expected_previous_submission","audited_baseline"));
        var allowed=new HashSet<>(required);allowed.add("mode");
        if(!plan.keySet().containsAll(required)||!allowed.containsAll(plan.keySet()))throw new IllegalArgumentException("Exact bounded plan fields required");
        for(var value:plan.values()) {
            String text=value.toString();
            if(text.isEmpty()||text.length()>4096||text.chars().anyMatch(c->c<32||c>126))
                throw new IllegalArgumentException("Bounded printable plan values required");
        }
        String started=ProcessHandle.current().info().startInstant().orElseThrow().toString();
        if(!Long.toString(ProcessHandle.current().pid()).equals(plan.getProperty("pid"))||!started.equals(plan.getProperty("started")))
            throw new IllegalArgumentException("Exact native PID and start instant required");
        String operation=plan.getProperty("operation_id");
        if(operation.length()>96||!operation.matches("[A-Za-z0-9][A-Za-z0-9._-]*")||CLAIMED.contains(operation))
            throw new IllegalArgumentException("Fresh bounded operation identity required");
        Path receipt=Path.of(plan.getProperty("receipt"));
        if(!receipt.isAbsolute()||Files.exists(receipt,LinkOption.NOFOLLOW_LINKS))throw new IllegalArgumentException("Fresh absolute receipt required");
        Class<?> target=null;
        for(Class<?> candidate:instrumentation.getAllLoadedClasses())if(candidate.getName().equals(TARGET)) {
            if(target!=null)throw new IllegalStateException("Ambiguous loaded observer class");
            target=candidate;
        }
        if(target==null||!instrumentation.isRedefineClassesSupported()||!instrumentation.isModifiableClass(target))
            throw new IllegalStateException("Loaded modifiable observer required");
        if(apply&&UNCERTAIN.contains(target))
            throw new IllegalStateException("Observer target has an uncertain update; separately reviewed recovery required");
        byte[] resource;
        try(var input=target.getResourceAsStream(RESOURCE)) {
            if(input==null)throw new IllegalStateException("Audited observer resource required");
            resource=input.readNBytes(LIMIT+1);
        }
        if(resource.length>LIMIT)throw new IllegalStateException("Observer resource bound");
        byte[] replacement=regular(Path.of(plan.getProperty("replacement")),LIMIT);
        String resourceHash=sha(resource),replacementHash=sha(replacement);
        if(!resourceHash.equals(plan.getProperty("resource_sha256"))||!replacementHash.equals(plan.getProperty("replacement_sha256"))||resourceHash.equals(replacementHash))
            throw new IllegalStateException("Exact different resource and replacement artifacts required");
        if(replacement.length<4||replacement[0]!=(byte)0xca||replacement[1]!=(byte)0xfe||replacement[2]!=(byte)0xba||replacement[3]!=(byte)0xbe)
            throw new IllegalStateException("Bounded JVM class artifact required");
        String loader=loaderId(target),source=codeSource(target),schema=schema(target);
        String previous=SUBMITTED.get(target);
        boolean knownSubmission=previous!=null;
        if(previous==null)previous=resourceHash;
        if(apply&&(source.equals("none")||loader.equals("bootstrap")||!plan.getProperty("audited_baseline").equals("true")
            ||!loader.equals(plan.getProperty("expected_loader_id"))||!source.equals(plan.getProperty("expected_code_source"))
            ||!schema.equals(plan.getProperty("expected_schema_sha256"))||!previous.equals(plan.getProperty("expected_previous_submission"))))
            throw new IllegalStateException("Audited cooperative lineage and exact loaded target identity required");
        String header="pid="+ProcessHandle.current().pid()+"\nstarted="+started+"\nclass="+TARGET+"\noperation_id="+operation+"\nmode="+mode
            +"\nloader_id="+loader+"\ncode_source="+source+"\nobserved_declared_schema_sha256="+schema
            +"\nresource_sha256="+resourceHash+"\nsubmitted_artifact_sha256="+replacementHash+"\nprevious_submission="+previous
            +"\nprevious_submission_known_by_this_agent="+knownSubmission
            +"\ntarget_update_uncertain="+UNCERTAIN.contains(target)
            +"\nloaded_method_bytes_verified=false\nexternal_transform_history_verified=false\nactive_old_frames_possible=true"
            +"\njournal_force_protocol=file_then_parent_before_redefine\nexternal_storage_failure_guaranteed=false\n";
        // This claim precedes the only instrumentation mutation. A missing
        // completion after timeout/failure is uncertain and must not be replayed.
        writeClaim(receipt,header+(apply?"status=claimed_before_redefinition\n":"status=claimed_before_inspection\n"));
        CLAIMED.add(operation);
        if(!apply) {
            append(receipt,"status=inspection_only_no_redefinition\ncompatibility_verified=false\n");
            return;
        }
        UNCERTAIN.add(target);
        try {
            instrumentation.redefineClasses(new ClassDefinition(target,replacement));
            SUBMITTED.put(target,replacementHash);
            append(receipt,"status=jvm_redefinition_returned\ncompatibility_verified_by_jvm=true\nloaded_method_bytes_verified=false\n");
            UNCERTAIN.remove(target);
        } catch(Exception|LinkageError failure) {
            try {append(receipt,"status=redefinition_outcome_uncertain_inspect_no_replay\n");}
            catch(Exception receiptFailure){failure.addSuppressed(receiptFailure);}
            throw failure;
        }
    }

    private static byte[] regular(Path path,int limit)throws Exception {
        if(!path.isAbsolute()||!Files.isRegularFile(path,LinkOption.NOFOLLOW_LINKS)||Files.size(path)>limit)
            throw new IllegalArgumentException("Absolute bounded regular artifact required");
        try(var input=Files.newInputStream(path,LinkOption.NOFOLLOW_LINKS)) {
            byte[] bytes=input.readNBytes(limit+1);
            if(bytes.length>limit)throw new IllegalArgumentException("Artifact byte bound");
            return bytes;
        }
    }
    private static String loaderId(Class<?> target) {
        var loader=target.getClassLoader();
        return loader==null?"bootstrap":loader.getClass().getName()+"@"+Integer.toHexString(System.identityHashCode(loader));
    }
    private static String codeSource(Class<?> target) {
        var source=target.getProtectionDomain().getCodeSource();
        return source==null||source.getLocation()==null?"none":source.getLocation().toExternalForm();
    }
    private static String schema(Class<?> target)throws Exception {
        var rows=new ArrayList<String>();
        rows.add("class "+target.getName()+" "+target.getModifiers()+" "+target.getSuperclass().getName());
        for(var iface:target.getInterfaces())rows.add("interface "+iface.getName());
        for(var field:target.getDeclaredFields())rows.add("field "+field.getName()+" "+field.getType().descriptorString()+" "+field.getModifiers());
        for(var method:target.getDeclaredMethods()) {
            var row=new StringBuilder("method ").append(method.getName()).append(" (");
            for(var type:method.getParameterTypes())row.append(type.descriptorString());
            row.append(")").append(method.getReturnType().descriptorString()).append(" ").append(method.getModifiers());
            rows.add(row.toString());
        }
        for(var constructor:target.getDeclaredConstructors()) {
            var row=new StringBuilder("constructor (");
            for(var type:constructor.getParameterTypes())row.append(type.descriptorString());
            rows.add(row.append(")V ").append(constructor.getModifiers()).toString());
        }
        java.util.Collections.sort(rows);
        return sha(String.join("\n",rows).getBytes(java.nio.charset.StandardCharsets.UTF_8));
    }
    private static void append(Path receipt,String text)throws Exception {
        try(var channel=FileChannel.open(receipt,StandardOpenOption.WRITE,StandardOpenOption.APPEND,LinkOption.NOFOLLOW_LINKS)) {
            writeForced(channel,text);
        }
        forceParent(receipt);
    }
    private static void writeClaim(Path receipt,String text)throws Exception {
        try(var channel=FileChannel.open(receipt,StandardOpenOption.WRITE,StandardOpenOption.CREATE_NEW,LinkOption.NOFOLLOW_LINKS)) {
            writeForced(channel,text);
        }
        forceParent(receipt);
        append(receipt,"claim_file_and_directory_force_returned=true\n");
    }
    private static void writeForced(FileChannel channel,String text)throws Exception {
        var buffer=ByteBuffer.wrap(text.getBytes(java.nio.charset.StandardCharsets.UTF_8));
        while(buffer.hasRemaining())channel.write(buffer);
        channel.force(true);
    }
    private static void forceParent(Path receipt)throws Exception {
        Path parent=receipt.getParent();
        if(parent==null||!Files.isDirectory(parent,LinkOption.NOFOLLOW_LINKS))throw new IllegalStateException("Real receipt directory required");
        try(var channel=FileChannel.open(parent,StandardOpenOption.READ)){channel.force(true);}
    }
    private static String sha(byte[] bytes)throws Exception {
        return HexFormat.of().formatHex(MessageDigest.getInstance("SHA-256").digest(bytes));
    }
}
