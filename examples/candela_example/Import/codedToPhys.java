public class codedToPhys implements JobTemplate
{
  public void execute (MCDRequestParameters inputParameters,
                       MCDJobApi jobHandler,
                       MCDLogicalLink link,
                       MCDSingleEcuJob apiJobObject) throws MCDException
  {
    MCDService service;
    MCDResult serviceResult, jobResult;
    MCDResultState resultState;
    MCDResponse serviceResponse, jobResponse;
    MCDResponseParameter value;
    
    /* Create the ReadMemoryByAddress service to be executed */
    service = link.createService( link.getDbObject().getDBLocation.getServices().
    getItemByName(ReadDataByAddress));
    
    /* Set variable request parameters of the service */
    service.setParameterValue("Address",
      inputParameters.getItemByName("Address").getValue());
      service.setParameterValue("Size",
      inputParameters.getItemByName("Size").getValue());
    
    /* Execute the service */
    resultState = service.executeSync();
    
    /* Fetch the service’s result and response */
    serviceResult = resultState.getResults().getItemByIndex(1);
    serviceResponse = serviceResult.getResponses().getItemByIndex(1);
    
    /* Create the job result and response */
    jobResult = apiJobObject.createResult(/* type, ... */);
    jobResponse = jobResult.getResponses().add(/*dbResponse*/);
    
    /* Copy the second service response parameter into the job response, the first is
    is the constant service-id parameter */
    jobResponse.getResponseParameters.addElementWithConent(
        serviceResponse.getResponseParameters().getItemByIndex(2));
    
    /* Delete the service instance */
    link.deleteComPrimitive(service);
    
    /* Send the final job result to the caller */
    jobHandler.sendFinalResult(jobResult);
    return;
  }
}
